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
/// any depth) or by a glob.
fn imports_command(tree: &str) -> bool {
    tree.match_indices("process::").any(|(at, _)| {
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

/// The lines of job `name` in a workflow: from `  name:` to the next two-space key, trimmed, blanks dropped.
fn job_lines(workflow: &str, name: &str) -> Vec<String> {
    let head = format!("  {name}:");
    let mut lines = workflow.lines().skip_while(|l| l.trim_end() != head);
    assert!(lines.next().is_some(), "no job {name}");
    lines
        .take_while(|l| l.is_empty() || l.starts_with("   "))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
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
    let refuse = line_of(&validate, r#"!= "$GITHUB_SHA""#);
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
    let refuse = line_of(&publish, "no longer points at");
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
