//! Repository rules checked over the source text (spec D3: one module starts processes).
//!
//! The rule: outside `#[cfg(test)]` items, `Command::new` and `process::Command` appear only in `src/spawn.rs`.
//! Every `.rs` file under `src/` is scanned except `src/bin/fake-client.rs` (the live tests' stand-in client, built
//! only with the `live` feature). An item is test-only when `#[cfg(test)]` (exactly) stands before it at any depth;
//! the item runs to its first `;` or its matching `}` outside brackets. Comments, strings and char literals are
//! blanked first, so a brace or the name inside them neither counts nor hides code.

use std::path::{Path, PathBuf};

const ALLOWED: &str = "src/spawn.rs";
const EXCLUDED: &str = "src/bin/fake-client.rs";
const NAMES: [&str; 2] = ["Command::new", "process::Command"];

fn ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The byte after a string, raw string or char literal starting at `i`, or none when none starts there.
fn literal_end(b: &[u8], i: usize) -> Option<usize> {
    let at = |k: usize| b.get(k).copied();
    match b[i] {
        b'"' => {
            let mut k = i + 1;
            while k < b.len() && b[k] != b'"' {
                k += if b[k] == b'\\' { 2 } else { 1 };
            }
            Some((k + 1).min(b.len()))
        }
        // r"..", r#".."#, br".."; not the end of an identifier such as `for`.
        b'r' if i == 0 || !ident(b[i - 1]) || (b[i - 1] == b'b' && (i < 2 || !ident(b[i - 2]))) => {
            let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
            if at(i + 1 + hashes) != Some(b'"') {
                return None;
            }
            let close: Vec<u8> = std::iter::once(b'"')
                .chain(std::iter::repeat_n(b'#', hashes))
                .collect();
            let body = i + 2 + hashes;
            let end = b[body..]
                .windows(close.len())
                .position(|w| w == close.as_slice())
                .map_or(b.len(), |p| body + p + close.len());
            Some(end)
        }
        // '\n', '\u{..}', '\'' or one char; anything else is a lifetime.
        b'\'' => {
            if at(i + 1) == Some(b'\\') {
                let close = b[i + 3..].iter().position(|&c| c == b'\'')?;
                return Some(i + 3 + close + 1);
            }
            let lead = at(i + 1)?;
            let len = match lead {
                0x00..=0x7f => 1,
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                _ => 4,
            };
            (at(i + 1 + len) == Some(b'\'')).then_some(i + 2 + len)
        }
        _ => None,
    }
}

/// `src` with comments, strings and char literals turned to spaces (line ends kept), byte for byte.
fn mask(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for c in &mut out[from..to] {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    };
    let mut i = 0;
    while i < b.len() {
        let next = b.get(i + 1).copied();
        let end = if b[i] == b'/' && next == Some(b'/') {
            b[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(b.len(), |p| i + p)
        } else if b[i] == b'/' && next == Some(b'*') {
            // Block comments nest.
            let (mut depth, mut k) = (0, i);
            while k < b.len() {
                if b[k..].starts_with(b"/*") {
                    depth += 1;
                    k += 2;
                } else if b[k..].starts_with(b"*/") {
                    depth -= 1;
                    k += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    k += 1;
                }
            }
            k
        } else if let Some(end) = literal_end(b, i) {
            end
        } else {
            i += 1;
            continue;
        };
        blank(&mut out, i, end);
        i = end;
    }
    String::from_utf8(out).expect("only whole literals and comments are blanked")
}

/// The code of `src` (masked) with every `#[cfg(test)]` item blanked: from the attribute to the item's first `;`
/// or its matching `}`, outside `()` and `[]`.
fn without_tests(src: &str) -> String {
    const GATE: &str = "#[cfg(test)]";
    let mut code = mask(src).into_bytes();
    let mut from = 0;
    while let Some(found) = String::from_utf8_lossy(&code[from..]).find(GATE) {
        let start = from + found;
        let (mut depth, mut end) = (0i32, code.len());
        for (k, &c) in code.iter().enumerate().skip(start + GATE.len()) {
            match c {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' => depth -= 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = k + 1;
                        break;
                    }
                }
                b';' if depth == 0 => {
                    end = k + 1;
                    break;
                }
                _ => {}
            }
        }
        for c in &mut code[start..end] {
            if *c != b'\n' {
                *c = b' ';
            }
        }
        from = end;
    }
    String::from_utf8(code).expect("only whole items are blanked")
}

/// The spans of the `use` declarations in masked `code`: the keyword to its `;`, holding only a use tree's
/// characters (so `impl Trait + use<'a>` is not one).
fn use_decls(code: &[u8]) -> Vec<(usize, usize)> {
    let tree = |c: &u8| ident(*c) || c.is_ascii_whitespace() || b":{},*".contains(c);
    let mut spans = Vec::new();
    let mut i = 0;
    while let Some(found) = code[i..].windows(3).position(|w| w == b"use") {
        let start = i + found;
        i = start + 3;
        let word =
            (start == 0 || !ident(code[start - 1])) && code.get(i).is_some_and(|c| !ident(*c));
        let Some(semi) = code[i..].iter().position(|&c| c == b';') else {
            break;
        };
        if word && code[i..i + semi].iter().all(tree) {
            spans.push((start, i + semi + 1));
            i += semi + 1;
        }
    }
    spans
}

/// A use tree with whitespace dropped, except one space between two identifiers (`Command as Other`).
fn compact(tree: &[u8]) -> String {
    let mut out = String::new();
    let mut gap = false;
    for &c in tree {
        if c.is_ascii_whitespace() {
            gap = true;
            continue;
        }
        if gap && ident(c) && out.bytes().last().is_some_and(ident) {
            out.push(' ');
        }
        gap = false;
        out.push(c as char);
    }
    out
}

/// Whether the word `name` starts at byte `at` of `text`.
fn word_at(text: &str, at: usize, name: &str) -> bool {
    let b = text.as_bytes();
    text[at..].starts_with(name)
        && (at == 0 || !ident(b[at - 1]))
        && b.get(at + name.len()).is_none_or(|c| !ident(*c))
}

/// Whether a compacted use tree imports `Command` from `process`: by name (aliased or not, grouped or nested at
/// any depth), by a glob, or through a renamed `process` module whose later paths the scan cannot follow.
fn imports_command(tree: &str) -> bool {
    let renamed = tree
        .match_indices("process as ")
        .any(|(at, _)| at == 0 || !ident(tree.as_bytes()[at - 1]));
    renamed
        || tree.match_indices("process::").any(|(at, _)| {
            if at > 0 && ident(tree.as_bytes()[at - 1]) {
                return false;
            }
            let rest = &tree[at + "process::".len()..];
            if !rest.starts_with('{') {
                return rest.starts_with('*') || word_at(rest, 0, "Command");
            }
            // The group's own braces, to the one that closes it.
            let mut depth = 0;
            let close = rest
                .char_indices()
                .find(|&(_, c)| {
                    depth += match c {
                        '{' => 1,
                        '}' => -1,
                        _ => 0,
                    };
                    depth == 0
                })
                .map_or(rest.len(), |(k, _)| k);
            let group = &rest[..close];
            group.contains('*')
                || group
                    .match_indices("Command")
                    .any(|(k, _)| word_at(group, k, "Command"))
        })
}

/// How many process-spawn sites the non-test code of `src` holds: each `use` that imports `std::process::Command`
/// in any form (named, aliased, grouped, glob), and each `Command::new` or `process::Command` path outside one.
fn spawn_sites(src: &str) -> usize {
    let mut code = without_tests(src).into_bytes();
    let mut imports = 0;
    for (start, end) in use_decls(&code) {
        if imports_command(&compact(&code[start..end])) {
            imports += 1;
        }
        code[start..end].fill(b' ');
    }
    let code = String::from_utf8(code).expect("only whole declarations are blanked");
    imports
        + NAMES
            .iter()
            .map(|name| code.matches(name).count())
            .sum::<usize>()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn scanner_skips_test_items_comments_and_strings() {
    let src = r##"
use std::process::Command;
// Command::new in a comment
/* nested /* Command::new */ still a comment */
fn real<'a>(x: &'a str) -> [u8; 2] {
    let _ = "Command::new {";
    let _ = r#"process::Command }"#;
    let _ = '{';
    Command::new(x);
    [0; 2]
}
#[cfg(test)]
use std::process::Command as Std;
#[cfg(test)]
mod tests {
    fn f() { let s = "}"; std::process::Command::new("x"); }
}
mod inner {
    #[cfg(test)]
    fn helper() { Command::new("y"); }
    fn kept() { Command::new("z"); }
}
"##;
    // `use std::process::Command`, `Command::new(x)` and `Command::new("z")`.
    assert_eq!(spawn_sites(src), 3);
    assert_eq!(
        mask("a \"b{\" 'c' // d\ne").len(),
        "a \"b{\" 'c' // d\ne".len()
    );
    assert!(!mask("x = \"{\";").contains('{'));
}

// Every way a `use` can bring in `Command` (aliased, grouped, nested, spaced, glob) is a site; other imports from
// std::process and names that only start with `Command` are not.
#[test]
fn scanner_flags_every_command_import() {
    for src in [
        "use std::process::{Command as Other};\nfn run() { Other::new(\"x\"); }",
        "use std::process::{self, Command};",
        "use std::process::*;",
        "pub(crate) use std::process::{\n    Child,\n    Command,\n};",
        "use std :: process :: { Stdio, Command as C };",
        "use std::{io, process::{Command, Stdio}};",
        "use std::process::{*};",
        "use std::{ process :: Command };",
        // A renamed module hides a later `use p::Command as Other`, so the rename itself is the site.
        "use std::process as p;\nuse p::Command as Other;\nfn f() { Other::new(\"x\"); }",
        "use std::{io, process as p};",
    ] {
        assert_eq!(spawn_sites(src), 1, "{src}");
    }
    for src in [
        "use std::process::{ExitCode, Stdio};",
        "use std::process::{self};\nfn f() -> process::ExitCode { process::ExitCode::SUCCESS }",
        "fn reuse() { let used = 1; }",
        "#[cfg(test)]\nuse std::process::{Command as Other};",
    ] {
        assert_eq!(spawn_sites(src), 0, "{src}");
    }
}

#[test]
fn process_spawn_only_in_spawn_rs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    files.sort();
    let mut found = Vec::new();
    for file in files {
        let name = file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if name == EXCLUDED {
            continue;
        }
        let sites = spawn_sites(&std::fs::read_to_string(&file).unwrap());
        if sites > 0 {
            found.push(name);
        }
    }
    // spawn.rs itself must be found, or the scan proves nothing.
    assert_eq!(found, [ALLOWED]);
}

// Release rules (spec D7): `main` is source-only; the marketplace installs the `dist` ref, which the release
// workflow writes as the tagged tree plus `scripts/bin/` and its `SHA256SUMS`.

fn repo_file(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}

/// `line` without its `#` comment: a `#` that starts a word outside quotes, as in YAML and sh. Enough for
/// workflow lines; `${#x}` and `a#b` are not comments.
fn strip_comment(line: &str) -> &str {
    let (mut single, mut double) = (false, false);
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        match c {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '#' if !single && !double && prev.is_whitespace() => return &line[..i],
            _ => {}
        }
        prev = c;
    }
    line
}

/// The code lines of job `name` in a workflow: from `  name:` to the next two-space key, comments stripped,
/// trimmed, blanks dropped.
fn job_lines(workflow: &str, name: &str) -> Vec<String> {
    let head = format!("  {name}:");
    let mut lines = workflow.lines().skip_while(|l| l.trim_end() != head);
    assert!(lines.next().is_some(), "no job {name}");
    lines
        .take_while(|l| l.is_empty() || l.starts_with("   "))
        .map(|l| strip_comment(l).trim())
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Index of the first code line holding `needle`, when it refuses: that line, or the next code line of the same
/// step, runs `exit 1`. None when no code line holds `needle` or it does not exit.
fn refusal(lines: &[String], needle: &str) -> Option<usize> {
    let at = lines.iter().position(|l| l.contains(needle))?;
    let step_end = lines[at + 1..]
        .iter()
        .position(|l| l.starts_with("- "))
        .map_or(lines.len(), |p| at + 1 + p);
    let exits = |l: &String| l.contains("exit 1");
    (exits(&lines[at]) || lines[at + 1..step_end].first().is_some_and(exits)).then_some(at)
}

/// Index of the first line of `lines` containing `needle`.
fn line_of(lines: &[String], needle: &str) -> usize {
    lines
        .iter()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line contains {needle:?} in {lines:#?}"))
}

#[test]
fn marketplace_pins_dist_ref() {
    let market: serde_json::Value =
        serde_json::from_str(&repo_file(".claude-plugin/marketplace.json")).unwrap();
    let plugins = market["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 1);
    assert_eq!(
        plugins[0]["source"],
        serde_json::json!({"source": "github", "repo": "D0n9X1n/mux-agents", "ref": "dist"})
    );
    // main never carries the packaged binaries.
    assert!(
        repo_file(".gitignore")
            .lines()
            .any(|l| l == "/scripts/bin/")
    );
}

#[test]
fn release_validate_checks_tag_sha() {
    let release = repo_file(".github/workflows/release.yml");
    let validate = job_lines(&release, "validate");
    let resolve = line_of(&validate, r#"git rev-parse "$TAG^{commit}""#);
    let refuse = refusal(&validate, r#"!= "$GITHUB_SHA""#).expect("validate refuses a moved tag");
    let gate = line_of(&validate, "scripts/release/gate.sh");
    assert!(resolve < refuse && refuse < gate, "{validate:#?}");
    // contents: write appears once, in publish; nothing runs PowerShell.
    assert_eq!(release.matches("contents: write").count(), 1);
    assert!(job_lines(&release, "publish").contains(&"contents: write".to_string()));
    assert!(!release.contains("pwsh") && !release.contains(".ps1"));
}

#[test]
fn release_publish_rechecks_remote_tag() {
    let release = repo_file(".github/workflows/release.yml");
    let publish = job_lines(&release, "publish");
    let assemble = line_of(&publish, "package.sh --assemble scripts/bin");
    let remote = line_of(
        &publish,
        r#"git ls-remote origin "refs/tags/$TAG" "refs/tags/$TAG^{}""#,
    );
    let refuse = refusal(&publish, "no longer points at").expect("publish refuses a moved tag");
    let add = line_of(&publish, "git add -f scripts/bin");
    let push = line_of(&publish, "git push");
    let create = line_of(&publish, "gh release create");
    assert!(
        assemble < remote && remote < refuse && refuse < add && add < push && push < create,
        "{publish:#?}"
    );
    // Every checkout leaves no credentials behind.
    let checkouts = release.matches("actions/checkout@").count();
    assert!(checkouts > 0);
    assert_eq!(
        release.matches("persist-credentials: false").count(),
        checkouts
    );
}

// The guard tests above must fail when a refusal is commented out or no longer exits.
#[test]
fn release_refusal_needs_live_exit() {
    let job = |body: &str| {
        let workflow =
            format!("jobs:\n  j:\n    steps:\n      - run: |\n{body}      - run: next\n");
        job_lines(&workflow, "j")
    };
    let needle = r#"!= "$GITHUB_SHA""#;
    let live = r#"          if [ "$sha" != "$GITHUB_SHA" ]; then echo refused; exit 1; fi"#;
    assert!(refusal(&job(&format!("{live}\n")), needle).is_some());
    assert!(
        refusal(
            &job(&format!("{}\n", live.replacen("if", "# if", 1))),
            needle
        )
        .is_none()
    );
    assert!(
        refusal(
            &job(&format!("{}\n", live.replace("exit 1", "true"))),
            needle
        )
        .is_none()
    );
    let split = "          if [ \"$sha\" != \"$GITHUB_SHA\" ]; then echo refused\n            exit 1\n          fi\n";
    assert!(refusal(&job(split), needle).is_some());
    let commented = "          if [ \"$sha\" != \"$GITHUB_SHA\" ]; then echo refused\n            # exit 1\n          fi\n";
    assert!(refusal(&job(commented), needle).is_none());
    // `${#x}`, a quoted `#` and a word holding `#` are code.
    assert_eq!(
        strip_comment(r#"n=${#x} "a # b" c#d # note"#),
        r#"n=${#x} "a # b" c#d "#
    );
}

#[test]
fn release_publish_keeps_dist_monotonic() {
    let release = repo_file(".github/workflows/release.yml");
    let publish = job_lines(&release, "publish");
    // One publish at a time, and a queued one is never cancelled.
    assert!(
        publish.contains(&"group: release-dist".to_string()),
        "{publish:#?}"
    );
    assert!(
        publish.contains(&"cancel-in-progress: false".to_string()),
        "{publish:#?}"
    );
    // The dist decision refuses before anything is added or pushed; the push leases the dist it read.
    let decide = refusal(&publish, "scripts/release/gate.sh --dist")
        .expect("publish refuses an older release");
    let add = line_of(&publish, "git add -f scripts/bin");
    let push = line_of(&publish, "git push");
    assert!(decide < add && add < push, "{publish:#?}");
    assert!(
        publish[push].contains("--force-with-lease=refs/heads/dist:"),
        "{}",
        publish[push]
    );
    assert!(!release.contains(" --force ") && !release.contains("push --force\n"));
}

// A token on a command line is visible to every process on the runner; git reads it from the environment instead.
fn assert_token_off_argv(path: &str) {
    for line in repo_file(path).lines().map(strip_comment) {
        for bad in ["x-access-token:$", "${{ secrets", "TOKEN@", "TOKEN}@"] {
            assert!(!line.contains(bad), "{path}: {bad:?} in {line}");
        }
        for (at, _) in line.match_indices("://") {
            let url: String = line[at..]
                .chars()
                .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\''))
                .collect();
            assert!(!url.contains('@'), "{path}: credentials in URL {url}");
        }
    }
}

#[test]
fn release_token_never_in_argv() {
    assert_token_off_argv(".github/workflows/release.yml");
}

// The wiki publish workflow: the sh publisher mirrors wiki/ into a plain clone; git reads the token from the
// credential helper environment on the clone and the push, never from a URL.
#[test]
fn wiki_publish_runs_sh_with_token_off_argv() {
    let wiki = repo_file(".github/workflows/publish-wiki.yml");
    let publish = job_lines(&wiki, "publish");
    let clone = line_of(
        &publish,
        r#"git clone "https://github.com/${GITHUB_REPOSITORY}.wiki.git" wiki-repo"#,
    );
    let mirror = line_of(
        &publish,
        r#"sh scripts/wiki/publish.sh wiki wiki-repo "$GITHUB_SHA""#,
    );
    let push = line_of(&publish, "git -C wiki-repo push origin HEAD:master");
    assert!(clone < mirror && mirror < push, "{publish:#?}");
    // The push runs exactly when the mirror step reports a change: its id and the push's condition must agree.
    let id = publish
        .iter()
        .position(|l| l == "id: mirror")
        .unwrap_or_else(|| panic!("no step has exactly `id: mirror`: {publish:#?}"));
    let gate = line_of(&publish, "if: steps.mirror.outputs.changed == 'true'");
    assert!(
        clone < id && id < mirror && mirror < gate && gate < push,
        "{publish:#?}"
    );
    // No other step condition can skip the clone, mirror or push.
    assert_eq!(
        publish
            .iter()
            .skip_while(|l| !l.starts_with("steps:"))
            .filter(|l| l.starts_with("if:"))
            .count(),
        1,
        "{publish:#?}"
    );
    // The helper is set on the clone's and the push's steps only, not the job: the publisher never sees the token.
    let helper = r#"GIT_CONFIG_VALUE_1: '!f() { echo username=x-access-token; echo "password=$GH_TOKEN"; }; f'"#;
    let helpers: Vec<usize> = publish
        .iter()
        .enumerate()
        .filter(|(_, l)| l.as_str() == helper)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(helpers.len(), 2, "{publish:#?}");
    assert!(
        helpers[0] < clone && clone < mirror && mirror < helpers[1] && helpers[1] < push,
        "{publish:#?}"
    );
    assert_token_off_argv(".github/workflows/publish-wiki.yml");
}

// Workflow rules over every file in .github/workflows, read as text: two-space YAML, one key per line. A block is a
// key line and every line after it indented deeper; comments are stripped and blank lines dropped first.

const CI: &str = ".github/workflows/ci.yml";
const RELEASE: &str = ".github/workflows/release.yml";
const WIKI: &str = ".github/workflows/publish-wiki.yml";

/// Every workflow as (path from the repo root, text), sorted by path.
fn workflows() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut out: Vec<(String, String)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".yml") || n.ends_with(".yaml"))
        .map(|n| {
            let path = format!(".github/workflows/{n}");
            let text = repo_file(&path);
            (path, text)
        })
        .collect();
    out.sort();
    out
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The code lines of a workflow, indentation kept: comments stripped, trailing blanks trimmed, blank lines dropped.
fn code_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| strip_comment(l).trim_end().to_string())
        .filter(|l| !l.trim().is_empty())
        .collect()
}

/// The first block whose key line is exactly `head`, indentation included; empty when there is none.
fn block(lines: &[String], head: &str) -> Vec<String> {
    let Some(at) = lines.iter().position(|l| l == head) else {
        return Vec::new();
    };
    let depth = indent(head);
    lines[at..at + 1]
        .iter()
        .chain(lines[at + 1..].iter().take_while(|l| indent(l) > depth))
        .cloned()
        .collect()
}

/// The job names of a workflow, in order.
fn jobs(lines: &[String]) -> Vec<String> {
    block(lines, "jobs:")
        .iter()
        .skip(1)
        .filter(|l| indent(l) == 2)
        .map(|l| l.trim().trim_end_matches(':').to_string())
        .collect()
}

/// Job `name`'s block, looked up under `jobs:` only.
fn job(lines: &[String], name: &str) -> Vec<String> {
    let found = block(&block(lines, "jobs:"), &format!("  {name}:"));
    assert!(!found.is_empty(), "no job {name}");
    found
}

/// A job's steps, each its `- ` line and the lines under it.
fn steps(job: &[String]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    for line in block(job, "    steps:").iter().skip(1) {
        if line.starts_with("      - ") {
            out.push(vec![line.clone()]);
        } else if let Some(step) = out.last_mut() {
            step.push(line.clone());
        }
    }
    out
}

/// The value of a step's own key (`uses`, `run`, `shell`, ...), not one nested under `with:` or `env:`.
fn step_value<'a>(step: &'a [String], key: &str) -> Option<&'a str> {
    let head = format!("{key}:");
    let first = step[0].trim_start().strip_prefix("- ").unwrap();
    std::iter::once(first)
        .chain(
            step[1..]
                .iter()
                .filter(|l| indent(l) == 8)
                .map(|l| l.trim()),
        )
        .find_map(|l| l.strip_prefix(head.as_str()).map(str::trim))
}

/// A job's `needs:`, as names.
fn needs(job: &[String]) -> Vec<String> {
    job.iter()
        .filter(|l| indent(l) == 4)
        .find_map(|l| l.trim().strip_prefix("needs:"))
        .map(|v| {
            v.trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

// A failing step that does not fail the job is a check that cannot go red.
#[test]
fn no_continue_on_error() {
    for (path, text) in workflows() {
        for line in code_lines(&text) {
            assert!(!line.contains("continue-on-error"), "{path}: {line}");
        }
    }
}

// Nothing is published unless the tag passed validate and every binary built.
#[test]
fn publish_needs_validate_and_build() {
    let release = code_lines(&repo_file(RELEASE));
    assert_eq!(jobs(&release), ["validate", "build", "publish"]);
    assert!(needs(&job(&release, "validate")).is_empty());
    assert_eq!(needs(&job(&release, "build")), ["validate"]);
    assert_eq!(needs(&job(&release, "publish")), ["validate", "build"]);
}

// Each workflow reads contents at the top; a write grant lives only on the one job that pushes.
#[test]
fn permissions_minimal_per_job() {
    const WRITERS: [(&str, &str); 2] = [(WIKI, "publish"), (RELEASE, "publish")];
    for (path, text) in workflows() {
        let lines = code_lines(&text);
        assert_eq!(
            block(&lines, "permissions:").join("\n"),
            "permissions:\n  contents: read",
            "{path}"
        );
        for line in &lines {
            assert!(
                !line.contains("write-all") && !line.contains("read-all"),
                "{path}: {line}"
            );
        }
        for name in jobs(&lines) {
            let grants = block(&job(&lines, &name), "    permissions:");
            let writes: Vec<&str> = grants
                .iter()
                .skip(1)
                .map(|l| l.trim())
                .filter(|g| !g.ends_with(": read") && !g.ends_with(": none"))
                .collect();
            let want: &[&str] = if WRITERS.contains(&(path.as_str(), name.as_str())) {
                &["contents: write"]
            } else {
                &[]
            };
            assert_eq!(writes, want, "{path} job {name}");
        }
    }
}

// A persisted checkout token is readable by every later step of the job.
#[test]
fn checkout_persist_credentials_false() {
    for (path, text) in workflows() {
        let lines = code_lines(&text);
        // Every checkout in the text is a step the parser found, so none escapes the check.
        let mut seen = 0;
        let total = lines
            .iter()
            .filter(|l| l.contains("actions/checkout@"))
            .count();
        for name in jobs(&lines) {
            for step in steps(&job(&lines, &name)) {
                if step_value(&step, "uses").is_some_and(|u| u.starts_with("actions/checkout@")) {
                    seen += 1;
                    let with = block(&step, "        with:");
                    assert!(
                        with.iter()
                            .any(|l| l.trim() == "persist-credentials: false"),
                        "{path} job {name}: {step:#?}"
                    );
                }
            }
        }
        assert!(
            seen > 0 && seen == total,
            "{path}: {seen} of {total} checkouts"
        );
    }
}

// CI runs on every pull request and every push to main; a release on a version tag; the wiki on main and by hand.
#[test]
fn required_triggers() {
    let paths: Vec<String> = workflows().into_iter().map(|(p, _)| p).collect();
    assert_eq!(paths, [CI, WIKI, RELEASE]);
    let on = |path: &str| block(&code_lines(&repo_file(path)), "on:").join("\n");
    assert_eq!(
        on(CI),
        "on:\n  pull_request:\n  push:\n    branches: [main]"
    );
    assert_eq!(
        on(RELEASE),
        "on:\n  push:\n    tags: ['v[0-9]+.[0-9]+.[0-9]+']"
    );
    assert_eq!(
        on(WIKI),
        "on:\n  push:\n    branches: [main]\n  workflow_dispatch:"
    );
}

// Branch protection requires lint, test (<os>) for three OSes and integration (<os>) for two: the job ids and matrices
// make those names, and a job-level name: would rename them.
#[test]
fn ci_test_job_named_test() {
    let ci = code_lines(&repo_file(CI));
    let names = jobs(&ci);
    for want in ["lint", "test", "integration"] {
        assert!(names.iter().any(|n| n == want), "{names:?}");
    }
    for name in &names {
        let renamed = job(&ci, name)
            .iter()
            .any(|l| indent(l) == 4 && l.trim_start().starts_with("name:"));
        assert!(!renamed, "job {name} has a name:");
    }
    let lint = job(&ci, "lint");
    assert!(lint.iter().any(|l| l == "    runs-on: ubuntu-latest"));
    assert!(block(&lint, "    strategy:").is_empty());
    for (name, oses) in [
        ("test", "[ubuntu-latest, macos-latest, windows-latest]"),
        ("integration", "[ubuntu-latest, macos-latest]"),
    ] {
        let j = job(&ci, name);
        assert_eq!(
            block(&j, "    strategy:").join("\n"),
            format!("    strategy:\n      fail-fast: false\n      matrix:\n        os: {oses}"),
            "job {name}"
        );
        assert!(
            j.iter().any(|l| l == "    runs-on: ${{ matrix.os }}"),
            "job {name}"
        );
    }
}

// What each CI job runs, in order: the Rust gates and the shims on every OS, the live, release and wiki suites on
// Linux and macOS, the scripts' lint, and a check of every release target.
#[test]
fn ci_jobs_run_cargo_and_sh() {
    let ci = repo_file(CI);
    let ordered = |name: &str, wants: &[&str]| {
        let lines = job_lines(&ci, name);
        let at: Vec<usize> = wants.iter().map(|w| line_of(&lines, w)).collect();
        assert!(at.windows(2).all(|p| p[0] < p[1]), "job {name}: {at:?}");
    };
    ordered(
        "test",
        &[
            "cargo fmt --check",
            "cargo clippy --all-targets --features test-panic --locked -- -D warnings",
            "cargo test --features test-panic --locked",
            "cargo build --release --locked",
            "cargo test --release --features test-panic --locked --test panic",
        ],
    );
    let test = job_lines(&ci, "test");
    line_of(&test, "sh tests/shim.sh");
    line_of(&test, r"cmd //d //c 'tests\shim.cmd'");
    line_of(&test, r"cmd //d //c 'scripts\mux-agent.cmd' list");
    line_of(&test, r#"echo "TEMP=$RUNNER_TEMP" >> "$GITHUB_ENV""#);
    ordered(
        "integration",
        &[
            "sh scripts/dev/tmux32.sh",
            "cargo test --features live,test-panic --locked --test live",
            "sh tests/release.sh",
            "sh tests/wiki.sh",
        ],
    );
    ordered(
        "lint",
        &[
            "./actionlint",
            "jq . .claude-plugin/plugin.json .claude-plugin/marketplace.json",
            "xargs -0 shellcheck",
            "@anthropic-ai/claude-code@2.1.310",
            "claude plugin validate .",
            "claude plugin test .",
        ],
    );
    let cross = job_lines(&ci, "cross");
    line_of(
        &cross,
        r#"cargo check --all-targets --locked --target "$target""#,
    );
    for target in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-musl",
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
    ] {
        line_of(&cross, target);
    }
}

// No PowerShell anywhere: none named, and none by default, since a Windows run step without a shell runs pwsh.
#[test]
fn no_pwsh_in_workflows() {
    for (path, text) in workflows() {
        let lower = text.to_lowercase();
        for bad in ["pwsh", "powershell", ".ps1", "install-module"] {
            assert!(!lower.contains(bad), "{path} names {bad}");
        }
        let lines = code_lines(&text);
        for name in jobs(&lines) {
            let j = job(&lines, &name);
            // A job runs on Windows when its runs-on or its matrix names a Windows runner.
            let on_windows = j
                .iter()
                .filter(|l| l.trim_start().starts_with("runs-on:"))
                .chain(block(&j, "    strategy:").iter())
                .any(|l| l.contains("windows-latest") || l.contains("windows-20"));
            if !on_windows {
                continue;
            }
            for step in steps(&j) {
                if step_value(&step, "run").is_some() {
                    assert_eq!(
                        step_value(&step, "shell"),
                        Some("bash"),
                        "{path} job {name}: {step:#?}"
                    );
                }
            }
        }
    }
}

// Docs, version and repo rules (0.2.0): one version everywhere, nothing needs PowerShell, the README and the skill
// name every platform, the wiki comes in English and Chinese twins, and the checks ported from the Pester suites of
// a96b866 (icons, action pins, export-ignore, merge procedure, old name, wiki) live on here.

const OWNER_WIKI: &str = "https://github.com/D0n9X1n/mux-agents/wiki/";

/// Runs git in the repository: (exit 0, stdout).
fn git(args: &[&str]) -> (bool, String) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn tracked() -> Vec<String> {
    git(&["ls-files"]).1.lines().map(String::from).collect()
}

/// A file's lines, CR dropped.
fn file_lines(path: &str) -> Vec<String> {
    repo_file(path)
        .lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect()
}

fn json_file(path: &str) -> serde_json::Value {
    serde_json::from_str(&repo_file(path)).unwrap()
}

/// `version` of Cargo.toml's [package] table.
fn cargo_version() -> String {
    let mut in_package = false;
    for line in file_lines("Cargo.toml") {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package && let Some(v) = line.strip_prefix("version = \"") {
            return v.trim_end_matches('"').to_string();
        }
    }
    panic!("no [package] version in Cargo.toml");
}

/// Whether a line names PowerShell in any spelling: the shell, its files, its module tooling or Pester.
fn names_powershell(line: &str) -> bool {
    let l = line.to_lowercase();
    [
        "pwsh",
        "powershell",
        ".ps1",
        ".psm1",
        "install-module",
        "pester",
    ]
    .iter()
    .any(|w| l.contains(w))
}

/// The README may name PowerShell only to say none is needed.
fn readme_line_ok(line: &str) -> bool {
    let l = line.to_lowercase();
    l.contains("no powershell")
        && !["pwsh", ".ps1", ".psm1", "install-module", "pester"]
            .iter()
            .any(|w| l.contains(w))
}

/// The one PowerShell line the skill keeps: how a PowerShell caller runs the entry.
const SKILL_PWSH_NOTE: &str = "The commands below, and the Reply and Listen lines, run as-is in the Bash tool. In PowerShell, prefix `& ` (`& \"<exe>\" send parent …`).";

/// Hooks and their tests may name the old `mux-agent.ps1` (spawn detection keeps accepting a 0.1 command line) and
/// Claude Code's `PowerShell` tool, whose calls the pane watches as it watches Bash.
fn hook_line_ok(line: &str) -> bool {
    line.to_lowercase().contains("mux-agent.ps1")
        || line.contains(r"\.ps1")
        || line.contains("'PowerShell'")
        || line.contains("Bash or PowerShell")
}

#[test]
fn plugin_version_is_0_2_0_and_matches_cargo() {
    assert_eq!(json_file(".claude-plugin/plugin.json")["version"], "0.2.0");
    assert_eq!(cargo_version(), "0.2.0");
    let lock = repo_file("Cargo.lock").replace("\r\n", "\n");
    assert!(
        lock.contains("name = \"mux-agent\"\nversion = \"0.2.0\"\n"),
        "Cargo.lock does not hold mux-agent 0.2.0"
    );
}

#[test]
fn no_pwsh_in_readme_skill_contributing_hooks() {
    let mut bad = Vec::new();
    for line in file_lines("README.md") {
        if names_powershell(&line) && !readme_line_ok(&line) {
            bad.push(format!("README.md: {line}"));
        }
    }
    for line in file_lines("skills/mux-agents/SKILL.md") {
        if names_powershell(&line) && line != SKILL_PWSH_NOTE {
            bad.push(format!("SKILL.md: {line}"));
        }
    }
    for path in ["CONTRIBUTING.md", ".github/pull_request_template.md"] {
        for line in file_lines(path) {
            if names_powershell(&line) {
                bad.push(format!("{path}: {line}"));
            }
        }
    }
    for path in tracked().iter().filter(|p| {
        p.starts_with("hooks/") || p.starts_with("tests/claude/") || p.starts_with("types/")
    }) {
        for line in file_lines(path) {
            if names_powershell(&line) && !hook_line_ok(&line) {
                bad.push(format!("{path}: {line}"));
            }
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
    // The skill keeps its note; the pane runs the shim, never PowerShell; spawn detection still takes 0.1 lines.
    assert!(file_lines("skills/mux-agents/SKILL.md").contains(&SKILL_PWSH_NOTE.to_string()));
    let register = repo_file("hooks/register.tsx");
    assert!(register.contains(
        r"return ['cmd.exe', '/d', '/c', `${root}\\scripts\\mux-agent.cmd`, 'list', '-All']"
    ));
    assert!(register.contains("return ['sh', `${root}/scripts/mux-agent`, 'list', '-All']"));
    assert!(repo_file("hooks/board.ts").contains(r"mux-agent(\.ps1|\.cmd|\.exe)?"));
}

#[test]
fn readme_skill_name_every_platform_and_mux() {
    let readme = file_lines("README.md");
    let platforms = "- rmux 0.10 or later on macOS, Linux or Windows, or tmux 3.2 or later on macOS or Linux, with the caller inside a pane.";
    assert!(
        readme.contains(&platforms.to_string()),
        "README requirements line"
    );
    assert!(
        readme.iter().any(|l| l.contains("macOS")
            && l.contains("Linux")
            && l.contains("Windows")
            && l.contains("no PowerShell")),
        "README says it runs natively without PowerShell"
    );
    let skill = file_lines("skills/mux-agents/SKILL.md");
    let when = skill
        .iter()
        .find(|l| l.starts_with("Use this skill inside "))
        .expect("skill's when-to-use line");
    for word in ["rmux 0.10+ pane (Windows, Linux or macOS)", "tmux 3.2"] {
        assert!(when.contains(word), "skill line lacks {word}: {when}");
    }
}

#[test]
fn tagline_shared() {
    let readme = file_lines("README.md");
    assert_eq!(readme[2], "# mux-agents");
    // The tagline is the first paragraph after the title.
    let tagline = &readme[4];
    assert!(!tagline.is_empty() && readme[3].is_empty() && readme[5].is_empty());
    assert_eq!(
        json_file(".claude-plugin/plugin.json")["description"],
        *tagline
    );
    assert_eq!(
        json_file(".claude-plugin/marketplace.json")["plugins"][0]["description"],
        *tagline
    );
}

/// Every `](target)` and `src="target"` in a Markdown text.
fn link_targets(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in text.match_indices("](") {
        let rest = &text[at + 2..];
        let end = rest.find([')', ' ', '\n', '\t']).unwrap_or(rest.len());
        out.push(rest[..end].to_string());
    }
    for (at, _) in text.match_indices("src=\"") {
        let rest = &text[at + 5..];
        out.push(rest[..rest.find('"').unwrap_or(rest.len())].to_string());
    }
    out
}

#[test]
fn readme_links_absolute() {
    // A relative link breaks on the marketplace page and in a packaged copy (assets/ is export-ignored).
    let targets = link_targets(&repo_file("README.md"));
    assert!(!targets.is_empty());
    let relative: Vec<&String> = targets
        .iter()
        .filter(|t| !t.starts_with("https://"))
        .collect();
    assert!(relative.is_empty(), "{relative:?}");
    // The spawn example passes the brief an agent caller needs.
    assert_eq!(
        file_lines("README.md")
            .iter()
            .filter(|l| l.starts_with("| `spawn` |") && l.contains("-BriefFile"))
            .count(),
        1
    );
}

/// The wiki pages, by name without `.md`, sorted. wiki/ is flat: the mirror publishes top-level pages only.
fn wiki_pages() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("wiki");
    let mut pages = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().into_string().unwrap();
        assert!(
            entry.file_type().unwrap().is_file(),
            "wiki/{name} is not a file"
        );
        let page = name.strip_suffix(".md");
        pages.push(page.unwrap_or_else(|| panic!("wiki/{name}")).to_string());
    }
    pages.sort();
    pages
}

fn twin(page: &str) -> String {
    match page.strip_suffix("-zh-CN") {
        Some(english) => english.to_string(),
        None => format!("{page}-zh-CN"),
    }
}

fn wiki_text(page: &str) -> String {
    repo_file(&format!("wiki/{page}.md")).replace("\r\n", "\n")
}

#[test]
fn wiki_pairs_and_links() {
    let pages = wiki_pages();
    let (zh, en): (Vec<&String>, Vec<&String>) = pages.iter().partition(|p| p.ends_with("-zh-CN"));
    assert!(!en.is_empty() && !zh.is_empty());
    let mut bad = Vec::new();
    for page in &pages {
        let other = twin(page);
        if !pages.contains(&other) {
            bad.push(format!("{page}: no twin {other}"));
        }
        let text = wiki_text(page);
        let head: String = text.lines().take(5).collect::<Vec<_>>().join("\n");
        if !head.contains(&format!("({other})"))
            && !head.contains(&format!("({OWNER_WIKI}{other})"))
        {
            bad.push(format!(
                "{page}: no link to {other} in its first five lines"
            ));
        }
        for target in link_targets(&text) {
            let scheme = target.split_once(':').is_some_and(|(s, _)| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
            });
            if page.starts_with("Release-") && !target.starts_with("https://") {
                // Release notes become a release body on github.com, where a wiki-relative link breaks.
                bad.push(format!("{page}: relative link {target}"));
            } else if !scheme && !target.starts_with('#') {
                let name = target.split('#').next().unwrap();
                if !pages.iter().any(|p| p == name) {
                    bad.push(format!("{page}: broken link {target}"));
                }
            }
        }
    }
    // Home links every page of its language.
    for (home, list) in [("Home", &en), ("Home-zh-CN", &zh)] {
        let targets = link_targets(&wiki_text(home));
        for page in list.iter().filter(|p| p.as_str() != home) {
            if !targets.iter().any(|t| t == *page) {
                bad.push(format!("{home} does not link {page}"));
            }
        }
    }
    let nested: Vec<String> = git(&["ls-files", "wiki"])
        .1
        .lines()
        .filter(|p| p.matches('/').count() > 1)
        .map(String::from)
        .collect();
    assert!(nested.is_empty(), "{nested:?}");
    assert!(bad.is_empty(), "{bad:#?}");
}

/// The code spans and numbers of a line, sorted and deduplicated: `` `[^`]+` `` and `\d+(\.\d+)*`.
fn spans_and_numbers(line: &str) -> Vec<String> {
    let c: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '`' {
            if let Some(len) = c[i + 1..].iter().position(|&x| x == '`').filter(|&n| n > 0) {
                out.push(c[i..i + len + 2].iter().collect());
                i += len + 2;
                continue;
            }
        } else if c[i].is_ascii_digit() {
            let start = i;
            while i < c.len() && c[i].is_ascii_digit() {
                i += 1;
            }
            while i + 1 < c.len() && c[i] == '.' && c[i + 1].is_ascii_digit() {
                i += 1;
                while i < c.len() && c[i].is_ascii_digit() {
                    i += 1;
                }
            }
            out.push(c[start..i].iter().collect());
            continue;
        }
        i += 1;
    }
    out.sort();
    out.dedup();
    out
}

/// What a line is: blank, a heading level, a table row, a list item, a numbered item, or text.
fn shape(line: &str) -> String {
    if line.trim().is_empty() {
        return "blank".into();
    }
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if hashes > 0 {
        return "#".repeat(hashes);
    }
    if line.starts_with('|') || line.starts_with('-') {
        return line[..1].to_string();
    }
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && line[digits..].starts_with('.') {
        return format!("{}.", &line[..digits]);
    }
    "text".into()
}

fn cjk(line: &str) -> bool {
    line.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// Whether a line is one Markdown link and nothing else: `[text](target)`.
fn only_link(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('[') else {
        return false;
    };
    let Some(close) = rest.find(']') else {
        return false;
    };
    let target = &rest[close + 1..];
    close > 0
        && target.len() > 2
        && target.starts_with('(')
        && target.ends_with(')')
        && !target[1..target.len() - 1].contains(')')
}

/// Lines (1-based) where `<page>-zh-CN` drifts from `page`: each line has the same shape, code spans and numbers and,
/// when it is prose, Chinese text; a fenced block is identical. A count mismatch is reported as line 0.
fn drift(page: &str) -> Vec<usize> {
    let en: Vec<String> = wiki_text(page).lines().map(String::from).collect();
    let zh: Vec<String> = wiki_text(&twin(page)).lines().map(String::from).collect();
    let mut out = Vec::new();
    if en.len() != zh.len() {
        out.push(0);
    }
    let mut fenced = false;
    for i in 0..en.len().max(zh.len()) {
        let a = en.get(i).map_or("", String::as_str);
        let b = zh.get(i).map_or("", String::as_str);
        if a.starts_with("```") || fenced {
            if a.starts_with("```") {
                fenced = !fenced;
            }
            if a != b {
                out.push(i + 1);
            }
            continue;
        }
        let t = b.trim();
        let separator =
            t.starts_with('|') && t.ends_with('|') && t.chars().all(|c| c == '|' || c == '-');
        let prose = !t.is_empty() && !only_link(t) && !separator;
        if shape(a) != shape(b)
            || spans_and_numbers(a) != spans_and_numbers(b)
            || (prose && !cjk(b))
        {
            out.push(i + 1);
        }
    }
    out
}

#[test]
fn zh_twins_match_line_counts() {
    let mut bad = Vec::new();
    for page in wiki_pages().iter().filter(|p| !p.ends_with("-zh-CN")) {
        let lines = drift(page);
        if !lines.is_empty() {
            bad.push(format!("{page}: {lines:?}"));
        }
        // Every design page records the Rust port in a section of its own, at the same line in both languages.
        if page.starts_with("Design-") {
            let en: Vec<String> = wiki_text(page).lines().map(String::from).collect();
            let zh: Vec<String> = wiki_text(&twin(page)).lines().map(String::from).collect();
            match en.iter().position(|l| l == "## Rust (0.2.0)") {
                Some(at) if zh.get(at).is_some_and(|l| l.starts_with("## Rust")) => {}
                _ => bad.push(format!(
                    "{page}: no ## Rust (0.2.0) section matched in its twin"
                )),
            }
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

// A tag can be moved; a commit cannot. The comment says which release the commit is.
#[test]
fn action_pins_are_40_hex_with_version_comment() {
    let mut seen = 0;
    for (path, text) in workflows() {
        for line in text.lines() {
            let t = line.trim_start();
            let t = t.strip_prefix("- ").unwrap_or(t).trim_start();
            let Some(rest) = t.strip_prefix("uses:") else {
                continue;
            };
            seen += 1;
            let (action, comment) = rest.split_once(" #").unwrap_or((rest, ""));
            let pinned = action.trim().split_once('@').is_some_and(|(name, sha)| {
                name.split('/').count() >= 2
                    && sha.len() == 40
                    && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            });
            let comment = comment.trim();
            let version = comment.strip_prefix('v').unwrap_or(comment);
            let version = version.split_whitespace().next().unwrap_or("");
            let versioned = version
                .split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
            assert!(pinned && versioned, "{path}: {line}");
        }
    }
    assert!(seen > 0);
}

#[test]
fn icons_png_sizes_svg_title() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon");
    for (name, view_box) in [
        ("mux-agents.svg", "0 0 512 512"),
        ("mux-agents-16.svg", "0 0 16 16"),
    ] {
        let svg = std::fs::read_to_string(dir.join(name)).unwrap();
        assert!(
            svg.trim_start()
                .starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""),
            "{name}"
        );
        assert!(svg.contains(&format!("viewBox=\"{view_box}\"")), "{name}");
        assert!(svg.contains("<title>mux-agents</title>"), "{name}");
        // Comments are not markup: drop them, then no script, outside reference, doctype, processing instruction
        // or event handler.
        let mut svg = svg;
        while let Some(start) = svg.find("<!--") {
            let end = svg[start..]
                .find("-->")
                .unwrap_or_else(|| panic!("{name}: unclosed comment"));
            svg.replace_range(start..start + end + 3, "");
        }
        let lower = svg.to_lowercase();
        for bad in [
            "<script",
            "href",
            "<!doctype",
            "<?",
            "<foreignobject",
            "<image",
            "<use",
        ] {
            assert!(!lower.contains(bad), "{name} holds {bad}");
        }
        assert!(
            !lower
                .split_whitespace()
                .any(|w| w.starts_with("on") && w.contains('=')),
            "{name} holds an event handler"
        );
        // url() only to an id inside the file.
        for (at, _) in lower.match_indices("url(") {
            assert!(
                lower[at + 4..].trim_start().starts_with('#'),
                "{name}: outside url()"
            );
        }
        // Only drawing elements.
        for (at, _) in svg.match_indices('<') {
            let tag: String = svg[at + 1..]
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            assert!(
                [
                    "svg",
                    "title",
                    "defs",
                    "linearGradient",
                    "stop",
                    "rect",
                    "path",
                    "circle"
                ]
                .contains(&tag.as_str()),
                "{name}: <{tag}>"
            );
        }
    }
    for size in [16u32, 32, 64, 128, 256, 512] {
        let png = std::fs::read(dir.join(format!("mux-agents-{size}.png"))).unwrap();
        assert_eq!(
            png[..8],
            [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
            "{size}"
        );
        // IHDR width and height, big-endian, at bytes 16 and 20.
        let be = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
        assert_eq!((be(16), be(20)), (size, size), "{size}");
    }
    assert_eq!(
        file_lines("README.md")[0],
        r#"<p align="center"><img src="https://raw.githubusercontent.com/D0n9X1n/mux-agents/main/assets/icon/mux-agents.svg" width="128" height="128" alt="mux-agents icon"></p>"#
    );
}

#[test]
fn export_ignore_rules() {
    let attributes = file_lines(".gitattributes");
    for want in [
        "scripts/release export-ignore",
        "scripts/wiki export-ignore",
        "scripts/dev export-ignore",
        "wiki export-ignore",
        "assets export-ignore",
        "scripts/mux-agent text eol=lf",
        "*.sh text eol=lf",
        "*.cmd text eol=crlf",
    ] {
        assert!(attributes.contains(&want.to_string()), "{want}");
    }
    // As git reads them, every line applied.
    let attr = |name: &str, path: &str| {
        let out = git(&["check-attr", name, "--", path]).1;
        out.trim().rsplit(": ").next().unwrap().to_string()
    };
    for path in [
        "scripts/release",
        "scripts/wiki",
        "scripts/dev",
        "wiki",
        "assets",
    ] {
        assert_eq!(attr("export-ignore", path), "set", "{path}");
    }
    for path in [
        "scripts/mux-agent",
        "scripts/mux-agent.cmd",
        "hooks",
        "skills",
        ".claude-plugin",
    ] {
        assert_eq!(attr("export-ignore", path), "unspecified", "{path}");
    }
    assert_eq!(attr("eol", "scripts/mux-agent"), "lf");
    assert_eq!(attr("eol", "scripts/release/gate.sh"), "lf");
    assert_eq!(attr("eol", "scripts/mux-agent.cmd"), "crlf");
    assert_eq!(attr("eol", "tests/shim.cmd"), "crlf");
}

/// The commands CI runs that a contributor runs before a pull request, each as ci.yml spells it.
const MUST_PASS: [&str; 13] = [
    "cargo fmt --check",
    "cargo clippy --all-targets --features test-panic --locked -- -D warnings",
    "cargo test --features test-panic --locked",
    "cargo build --release --locked",
    "cargo test --release --features test-panic --locked --test panic",
    "sh tests/shim.sh",
    "sh scripts/dev/tmux32.sh",
    "cargo test --features live,test-panic --locked --test live",
    "sh tests/release.sh",
    "sh tests/wiki.sh",
    "claude plugin validate .",
    "claude plugin test .",
    "git ls-files -z 'scripts/*.sh' scripts/mux-agent 'tests/*.sh' | xargs -0 shellcheck",
];

#[test]
fn contributing_and_pr_template_merge_sentences() {
    let contributing = file_lines("CONTRIBUTING.md");
    for want in [
        "1. `gh pr view <n> --json headRefOid,statusCheckRollup`",
        "2. Every check on that `headRefOid` is `SUCCESS`, and `lint`, `test (ubuntu-latest)`, `test (macos-latest)`, `test (windows-latest)`, `integration (ubuntu-latest)` and `integration (macos-latest)` are all present.",
        "3. `git fetch origin <sha>`, then `git push origin <sha>:main`, with `<sha>` the `headRefOid` from step 1. The push is not forced, so it lands only when `<sha>` builds on the current `main`; GitHub then marks the pull request merged.",
        "Commits are authored and committed as `D0n9X1n <D0n9x1n@outlook.com>`. A pull request lands by fast-forwarding `main` to its tested head, never through a GitHub merge, which would re-author it.",
    ] {
        assert!(
            contributing.contains(&want.to_string()),
            "CONTRIBUTING: {want}"
        );
    }
    let template = file_lines(".github/pull_request_template.md");
    for want in [
        "- [ ] `gh pr view <n> --json headRefOid,statusCheckRollup`: every check on headRefOid is SUCCESS; `lint`, `test (ubuntu-latest)`, `test (macos-latest)`, `test (windows-latest)`, `integration (ubuntu-latest)` and `integration (macos-latest)` are present",
        "- [ ] `git fetch origin <sha>`, then `git push origin <sha>:main`, with `<sha>` the checked headRefOid; not forced, so it lands only on the current `main`",
    ] {
        assert!(template.contains(&want.to_string()), "template: {want}");
    }
    for heading in [
        "## Summary",
        "## Type",
        "## Closes",
        "## Test plan",
        "## Merge checklist",
    ] {
        assert!(template.contains(&heading.to_string()), "{heading}");
    }
    // A GitHub merge re-authors; a forced push could land an untested head.
    for line in contributing.iter().chain(&template) {
        assert!(!line.contains("gh pr merge"), "{line}");
        if line.contains("git push") {
            let forced = line.contains("--force")
                || line
                    .split([' ', '`', '"', '\''])
                    .any(|w| w == "-f" || (w.starts_with('+') && w.len() > 1));
            assert!(!forced, "{line}");
        }
    }
    // Must-pass: every command CI runs, listed for contributors, and still in ci.yml.
    let text = repo_file("CONTRIBUTING.md");
    let ci = repo_file(CI);
    for cmd in MUST_PASS {
        assert!(text.contains(cmd), "CONTRIBUTING lacks {cmd}");
        assert!(ci.contains(cmd), "ci.yml lacks {cmd}");
    }
    assert!(text.contains("actionlint") && text.contains(r"tests\shim.cmd"));
    let plan = repo_file(".github/pull_request_template.md");
    assert!(plan.contains("cargo test --features test-panic --locked"));
}

#[test]
fn old_name_absent_outside_foundation_pages() {
    // Built from fragments, so this file does not match itself. The design record of the rename may name it.
    let old = ["r", "mux-agent|R", "muxAgent|R", "MUX_AGENTS"].concat();
    let (found, hits) = git(&[
        "grep",
        "-n",
        "-E",
        &old,
        "--",
        ".",
        ":(exclude,literal)wiki/Design-Foundation.md",
        ":(exclude,literal)wiki/Design-Foundation-zh-CN.md",
    ]);
    assert!(!found && hits.is_empty(), "{hits}");
}

#[test]
fn tracks_no_docs_path() {
    let (_, out) = git(&["ls-files", "--", "docs"]);
    assert!(out.is_empty(), "{out}");
}

#[test]
fn readme_notes_dist_pin() {
    // The marketplace installs ref dist, which the first 0.2.0 release creates.
    let readme = file_lines("README.md");
    assert!(
        readme.iter().any(|l| l.contains("`dist`")
            && l.contains("marketplace")
            && l.contains("first 0.2.0 release")),
        "README does not note the dist pin"
    );
}

#[test]
fn no_powershell_needed() {
    let files = tracked();
    let scripts: Vec<&String> = files
        .iter()
        .filter(|p| {
            [".ps1", ".psm1", ".psd1", ".cs"]
                .iter()
                .any(|e| p.ends_with(e))
        })
        .collect();
    assert!(scripts.is_empty(), "{scripts:?}");
    let mut bad = Vec::new();
    for path in files.iter().filter(|p| {
        ["scripts/", ".github/", ".claude-plugin/"]
            .iter()
            .any(|d| p.starts_with(d))
            || ["Cargo.toml", "CONTRIBUTING.md"].contains(&p.as_str())
    }) {
        let full = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
        let Ok(text) = std::fs::read_to_string(full) else {
            continue; // not text
        };
        for line in text.lines() {
            if names_powershell(line) {
                bad.push(format!("{path}: {line}"));
            }
        }
    }
    for path in files.iter().filter(|p| p.starts_with("hooks/")) {
        for line in file_lines(path) {
            if names_powershell(&line) && !hook_line_ok(&line) {
                bad.push(format!("{path}: {line}"));
            }
        }
    }
    for line in file_lines("README.md") {
        if names_powershell(&line) && !readme_line_ok(&line) {
            bad.push(format!("README.md: {line}"));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

// Upgrade path (spec D10): every release carries one version in its tag, plugin.json and Cargo.toml, and moves dist;
// a user upgrades with one command per client and keeps the state, mail and lock formats of 0.2.x.

/// Runs `sh scripts/release/gate.sh <args>` with a green CI push run for `sha`: (exit code, stdout).
#[cfg(unix)]
fn run_gate(args: &[&str]) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("mux-gate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let runs = dir.join("runs.json");
    std::fs::write(
        &runs,
        r#"[{"conclusion":"success","head_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","event":"push"}]"#,
    )
    .unwrap();
    let mut argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    argv.insert(3, runs.to_str().unwrap().to_string());
    let out = std::process::Command::new("sh")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/release/gate.sh"))
        .args(&argv)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

#[test]
fn release_gate_requires_tag_eq_plugin_eq_cargo_version() {
    // validate reads both versions and gives both to the gate.
    let release = repo_file(RELEASE);
    let validate = job_lines(&release, "validate");
    line_of(
        &validate,
        "version=$(jq -r .version .claude-plugin/plugin.json)",
    );
    let cargo = line_of(&validate, "cargo_version=$(");
    assert!(
        validate[cargo].contains("Cargo.toml"),
        "{}",
        validate[cargo]
    );
    let gate = line_of(
        &validate,
        r#"sh scripts/release/gate.sh "$TAG" "$sha" "$on_main" "$RUNNER_TEMP/runs.json" "$version" "$cargo_version""#,
    );
    assert!(cargo < gate, "{validate:#?}");
    #[cfg(unix)]
    {
        let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let ok = format!("release ok: v0.2.0 at {sha}");
        assert_eq!(
            run_gate(&["v0.2.0", sha, "true", "0.2.0", "0.2.0"]),
            (0, ok)
        );
        assert_eq!(
            run_gate(&["v0.2.0", sha, "true", "0.2.0", "0.2.1"]),
            (
                1,
                "refused: Cargo.toml version 0.2.1 does not match v0.2.0".into()
            )
        );
        assert_eq!(
            run_gate(&["v0.2.0", sha, "true", "0.2.1", "0.2.0"]),
            (
                1,
                "refused: plugin.json version 0.2.1 does not match v0.2.0".into()
            )
        );
        assert_eq!(
            run_gate(&["v0.2.0", sha, "true", "0.2.0", ""]),
            (
                1,
                "refused: Cargo.toml version  does not match v0.2.0".into()
            )
        );
        // Five arguments, as before Cargo.toml was checked, is a usage error.
        assert_eq!(run_gate(&["v0.2.0", sha, "true", "0.2.0"]).0, 2);
    }
}

#[test]
fn readme_has_one_line_upgrade_for_both_clis() {
    let readme = file_lines("README.md");
    for want in [
        "claude plugin update mux-agents@mux-agents",
        "copilot plugin update mux-agents",
    ] {
        assert!(
            readme.contains(&want.to_string()),
            "README lacks the line {want}"
        );
    }
}

#[test]
fn readme_shows_auto_update_opt_in() {
    // Claude Code leaves auto-update off for a third-party marketplace; the user turns it on in /plugin.
    let readme = file_lines("README.md");
    assert!(
        readme.iter().any(|l| l.contains("`/plugin`")
            && l.contains("**Marketplaces**")
            && l.contains("**Enable auto-update**")),
        "README does not show where to enable auto-update"
    );
    assert!(
        readme
            .iter()
            .any(|l| l.contains("auto-update") && l.contains("off by default")),
        "README does not say auto-update starts off"
    );
}

#[test]
fn release_moves_dist_with_new_plugin_version() {
    // publish reads the tagged plugin.json, lets the gate compare it with dist's, and only then commits dist.
    let release = repo_file(RELEASE);
    let publish = job_lines(&release, "publish");
    let dist_version = line_of(
        &publish,
        r#"dist_version=$(git show "$dist:.claude-plugin/plugin.json" | jq -r .version)"#,
    );
    let version = line_of(
        &publish,
        "version=$(jq -r .version .claude-plugin/plugin.json)",
    );
    let decide = line_of(
        &publish,
        r#"sh scripts/release/gate.sh --dist "$version" "$GITHUB_SHA" "$dist_version" "$dist_sha""#,
    );
    let commit = line_of(&publish, r#"commit -q -m "dist $TAG""#);
    assert!(
        dist_version < decide && version < decide && decide < commit,
        "{publish:#?}"
    );
    // And a real Claude Code moves an install to the new version on one update, from a relative source and on the
    // same ref.
    let upgrade = repo_file("tests/upgrade.sh");
    for case in ["migrate_relative_to_dist", "bump_on_same_ref"] {
        assert!(upgrade.contains(case), "tests/upgrade.sh lacks {case}");
    }
    assert!(upgrade.contains("command claude plugin update \"mux-agents@$mkt\""));
    assert!(!upgrade.contains("MUX_AGENTS_BIN="));
}

#[test]
fn state_and_lock_format_pinned() {
    // 0.2.x keeps the state root, the record, launcher and mail files, and the lock names, so an upgrade within
    // 0.2.x finds every agent and message and serializes with panes still on the older binary.
    let src = |path: &str| repo_file(path).replace("\r\n", "\n");
    let root = src("src/state/root.rs");
    assert!(root.contains("\"{}/.local/state/mux-agents/state\","));
    assert!(root.contains(r#"Ok(format!("{local}{sep}mux-agents\\state").into())"#));
    let lock = src("src/state/lock.rs");
    for want in [
        "crate::util::sha256_hex(key)[..32].to_owned()",
        r#"root.join(format!("{}.lock", key_hex(key)))"#,
        r#"format!(r"Local\MuxAgents-{}", key_hex(key))"#,
        r#"let above = [".local", "state", "mux-agents"];"#,
        r#"path.push("locks");"#,
        r#"PathBuf::from("/r/locks/ba7816bf8f01cfea414140de5dae2223.lock")"#,
        r#"r"Local\MuxAgents-ba7816bf8f01cfea414140de5dae2223""#,
    ] {
        assert!(lock.contains(want), "src/state/lock.rs lacks {want}");
    }
    assert!(src("src/agents/mod.rs").contains(r#"format!("{kind}|{}|{key}", caller.server_key)"#));
    let mail = src("src/agents/mail.rs");
    assert!(mail.contains(r#"mail.join(format!("{key}.jsonl"))"#));
    assert!(mail.contains(r#"mail.join(format!("{key}.offset"))"#));
    assert!(mail.contains(r#"Ok(format!("pane-{}", &pane[1..]))"#));
    let registry = src("src/agents/registry.rs");
    assert!(registry.contains(r#".join("record.json")"#));
    assert!(registry.contains(r#".join("launcher.json")"#));
    // The README promises it.
    assert!(
        file_lines("README.md")
            .iter()
            .any(|l| l.contains("0.2.x") && l.contains("state folder") && l.contains("lock")),
        "README does not say 0.2.x keeps the state and lock formats"
    );
}

/// The lines of the `## <heading>` section of a Markdown text, without the heading.
fn section(text: &str, heading: &str) -> Vec<String> {
    let head = format!("## {heading}");
    text.lines()
        .skip_while(|l| l.trim_end() != head)
        .skip(1)
        .take_while(|l| !l.starts_with("## "))
        .map(String::from)
        .collect()
}

#[test]
fn readme_upgrade_lifecycle() {
    // From 0.1: close the side bar and stop every agent first (the 0.1 mutex and the 0.2 lock files do not
    // serialize), then update, then restart the sessions; panes keep the binary path they started with.
    let readme = repo_file("README.md").replace("\r\n", "\n");
    let upgrade = section(&readme, "Upgrade");
    assert!(!upgrade.is_empty(), "README has no ## Upgrade section");
    let at = |needle: &str| {
        upgrade
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("## Upgrade lacks {needle}: {upgrade:#?}"))
    };
    let close = at("sidebar -Close");
    let stop = at("`stop`");
    let update = at("claude plugin update mux-agents@mux-agents");
    let restart = at("Restart");
    assert!(
        close < stop && stop < update && update < restart,
        "{upgrade:#?}"
    );
    at("absolute path");
    at("do not serialize");
    // The release notes say the same, in both languages.
    for page in ["Release-v0.2.0", "Release-v0.2.0-zh-CN"] {
        let text = wiki_text(page);
        for needle in [
            "sidebar -Close",
            "`stop`",
            "claude plugin update mux-agents@mux-agents",
            "copilot plugin update mux-agents",
        ] {
            assert!(text.contains(needle), "{page} lacks {needle}");
        }
    }
}
