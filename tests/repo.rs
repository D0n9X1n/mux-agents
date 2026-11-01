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
