//! A Copilot fork's session copy: `~/.copilot/session-state/<source>` becomes `<target>` (MuxAgents.ps1:585-639).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// `<home>/.copilot/session-state/<id>`.
pub fn session_folder(home: &Path, id: &str) -> PathBuf {
    home.join(".copilot").join("session-state").join(id)
}

/// Copies session `source` to `target` and returns whether the copy is usable. Both ids must be GUIDs. Top-level
/// `*.jsonl` lose a final unterminated segment that does not parse (a parsable one gets its LF); `*.yaml` are never
/// cut. In both, `source` becomes `target` and workspace.yaml loses its `mc_` keys; an `mc_` value the copy cannot
/// verify (see `without_mc`) refuses the copy. Subfolders are copied as they are. The copy is built and verified in
/// a uniquely named sibling temp folder, then renamed into place without replacing anything; a failure deletes only
/// the temp folder.
pub fn copy_session(home: &Path, source: &str, target: &str) -> bool {
    if !crate::checks::session_id(source) || !crate::checks::session_id(target) {
        return false;
    }
    let from = session_folder(home, source);
    // Without a source, making the temp folder would make session-state itself. A link is not followed.
    if !std::fs::symlink_metadata(&from).is_ok_and(|m| m.is_dir()) {
        return false;
    }
    let Ok(nonce) = crate::util::guid() else {
        return false;
    };
    let root = home.join(".copilot").join("session-state");
    let temp = root.join(format!(".{target}.{}.tmp", nonce.replace('-', "")));
    let built = std::fs::create_dir(&temp)
        .and_then(|()| build(&from, &temp, source, target))
        .and_then(|()| rename_noreplace(&temp, &root.join(target)));
    if built.is_err() {
        let _ = std::fs::remove_dir_all(&temp);
    }
    built.is_ok()
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

fn parses(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line).is_ok()
}

fn build(from: &Path, temp: &Path, source: &str, target: &str) -> io::Result<()> {
    copy_tree(from, temp)?;
    // Keyed by lowercase name: Windows names ignore case, and the checks below name the files in lowercase.
    let mut written: HashMap<String, String> = HashMap::new();
    for entry in std::fs::read_dir(temp)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let jsonl = name.ends_with(".jsonl");
        if !jsonl && !name.ends_with(".yaml") {
            continue;
        }
        let bytes = std::fs::read(&path)?;
        // ReadAllText drops a UTF-8 BOM, and WriteAllText writes none.
        let mut text =
            String::from_utf8_lossy(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
                .into_owned();
        if jsonl {
            let end = text.rfind('\n').map_or(0, |i| i + 1);
            if parses(&text[end..]) {
                text.push('\n');
            } else {
                text.truncate(end);
            }
        }
        text = text.replace(source, target);
        if name == "workspace.yaml" {
            text = without_mc(&text)
                .ok_or_else(|| invalid("workspace.yaml has an mc_ value the copy cannot drop"))?;
        }
        std::fs::write(&path, &text)?;
        if jsonl {
            let mut lines: Vec<&str> = text.split('\n').collect();
            lines.pop();
            if !lines.iter().all(|line| parses(line)) {
                return Err(invalid("copied JSONL line does not parse"));
            }
        }
        if text.contains(source) {
            return Err(invalid("copied file keeps the source id"));
        }
        written.insert(name, text);
    }
    let id_line = format!("id: {target}");
    let workspace = written.get("workspace.yaml").map_or("", String::as_str);
    if !workspace.split('\n').any(|line| line == id_line) {
        return Err(invalid("copied workspace.yaml lacks the target id"));
    }
    let events = written.get("events.jsonl").map_or("", String::as_str);
    if !events.contains(target) {
        return Err(invalid("copied events.jsonl lacks the target id"));
    }
    Ok(())
}

/// workspace.yaml without its top-level `mc_` keys, each with its whole block: the key line and every indented line
/// after it. Copilot CLI 1.0.110 writes `mc_pending_repository_warning:` as a mapping; dropping only the key line
/// leaves its children under the previous key, which Copilot then refuses to load. Only a non-blank, non-comment
/// line without indent ends a block. A blank or comment line met inside one is held: an indented line after it
/// drops it with the block; a top-level line or the end of the text keeps it.
///
/// `None` refuses what the filter cannot verify: an `mc_` key line that leaves a `{` or `[` open, a first top-level
/// line after an `mc_` key that is not a plain `key:` line (`- item`, `}`, `]`, ...), or any dropped line with a `&`.
/// An anchor dropped with the block would leave its aliases elsewhere pointing at nothing; a `&` that is not an
/// anchor (in quotes, say) is refused too, and the member starts fresh.
fn without_mc(text: &str) -> Option<String> {
    let mut kept = Vec::new();
    let mut held = Vec::new();
    let mut dropping = false;
    for line in text.split('\n') {
        let trimmed = line.trim();
        if dropping && (trimmed.is_empty() || trimmed.starts_with('#')) {
            held.push(line);
        } else if dropping && line.starts_with([' ', '\t']) {
            if line.contains('&') || held.iter().any(|h| h.contains('&')) {
                return None;
            }
            held.clear();
        } else {
            if dropping && !plain_key(line) {
                return None;
            }
            kept.append(&mut held);
            dropping = line.starts_with("mc_");
            if dropping && (open_flow(line) || line.contains('&')) {
                return None;
            }
            if !dropping {
                kept.push(line);
            }
        }
    }
    kept.append(&mut held);
    Some(kept.join("\n"))
}

/// A top-level line that starts a plain `key:` entry: a letter, digit or `_` first, and a `:` that ends the line or
/// is followed by a space.
fn plain_key(line: &str) -> bool {
    let line = line.trim_end();
    line.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
        && (line.ends_with(':') || line.contains(": "))
}

/// Whether `line` leaves a flow `{` or `[` open. shortcut: brackets inside quotes are counted too. A quoted `{` or
/// `[` refuses a valid value; a quoted `}` or `]` can hide an open one, and a closer that then reaches column 0 is
/// still refused by `plain_key`.
fn open_flow(line: &str) -> bool {
    let depth = line.chars().fold(0i32, |depth, c| match c {
        '{' | '[' => depth + 1,
        '}' | ']' => depth - 1,
        _ => depth,
    });
    depth > 0
}

/// Folders and files below `from` into `to`. A link (or junction) refuses the copy: Copilot makes none, and
/// following one could copy what lies outside the session.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let into = to.join(entry.file_name());
        if kind.is_dir() {
            std::fs::create_dir(&into)?;
            copy_tree(&entry.path(), &into)?;
        } else if kind.is_file() {
            // Copilot may still be appending; std opens with read and write sharing on Windows.
            let mut reader = std::fs::File::open(entry.path())?;
            let mut copy = std::fs::File::create_new(&into)?;
            io::copy(&mut reader, &mut copy)?;
        } else {
            return Err(invalid("the session holds a link"));
        }
    }
    Ok(())
}

/// Moves `from` to `to`, failing when anything is at `to`: a competing copy is never replaced.
#[cfg(target_os = "linux")]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let (from, to) = (
        std::ffi::CString::new(from.as_os_str().as_bytes())?,
        std::ffi::CString::new(to.as_os_str().as_bytes())?,
    );
    // The raw syscall: older glibc and musl have no renameat2 wrapper.
    // SAFETY: both paths are live NUL-terminated strings.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let (from, to) = (
        std::ffi::CString::new(from.as_os_str().as_bytes())?,
        std::ffi::CString::new(to.as_os_str().as_bytes())?,
    );
    // SAFETY: both paths are live NUL-terminated strings.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |path: &Path| -> Vec<u16> { path.as_os_str().encode_wide().chain([0]).collect() };
    let (from, to) = (wide(from), wide(to));
    // Without MOVEFILE_REPLACE_EXISTING the move fails when anything holds the target, as Directory.Move did.
    // SAFETY: both are live NUL-terminated UTF-16 strings.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 0)
    } != 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    const OLD: &str = "11111111-2222-4333-8444-555555555555";
    const NEW: &str = "12345678-9abc-4def-8123-456789abcdef";

    struct Session {
        home: TempDir,
        source: PathBuf,
        target: PathBuf,
    }

    // Damages a fresh session so its copy must fail.
    type Break = fn(&Session);

    fn session() -> Session {
        let home = TempDir::new();
        let source = session_folder(home.path(), OLD);
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("workspace.yaml"),
            format!("id: {OLD}\ncwd: /work\nmc_task_id: x\nsummary: s\n"),
        )
        .unwrap();
        std::fs::write(
            source.join("events.jsonl"),
            format!(
                "{{\"type\":\"session.start\",\"data\":{{\"sessionId\":\"{OLD}\"}}}}\n{{\"type\":\"user.message\"}}\n"
            ),
        )
        .unwrap();
        let target = session_folder(home.path(), NEW);
        Session {
            home,
            source,
            target,
        }
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    fn names(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    // Every file below `folder` with its bytes, so any change shows.
    fn tree(folder: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.push((path.clone(), Vec::new()));
                out.extend(tree(&path));
            } else {
                out.push((path.clone(), std::fs::read(&path).unwrap()));
            }
        }
        out.sort();
        out
    }

    #[test]
    fn copilot_copy_drops_an_mc_block_with_its_children() {
        // Copilot CLI 1.0.110: a nested mc_ mapping leaves with its indented lines; the next top-level key stays.
        let s = session();
        std::fs::write(
            s.source.join("workspace.yaml"),
            format!(
                "id: {OLD}\ncwd: /work\nremote_steerable: false\nmc_pending_repository_warning:\n  mc_session_id: {OLD}\n  message: 'not linked: no repo'\nmc_task_id: x\nsummary: s\n"
            ),
        )
        .unwrap();
        assert!(copy_session(s.home.path(), OLD, NEW));
        assert_eq!(
            read(&s.target.join("workspace.yaml")),
            format!("id: {NEW}\ncwd: /work\nremote_steerable: false\nsummary: s\n")
        );
    }

    // workspace.yaml as the copy writes it, from `body` written below the source's id line.
    fn copied_workspace(body: &str) -> String {
        let s = session();
        std::fs::write(
            s.source.join("workspace.yaml"),
            format!("id: {OLD}\n{body}"),
        )
        .unwrap();
        assert!(copy_session(s.home.path(), OLD, NEW));
        read(&s.target.join("workspace.yaml"))
    }

    #[test]
    fn copilot_copy_drops_an_mc_block_at_the_end() {
        assert_eq!(
            copied_workspace(&format!(
                "cwd: /work\nsummary: s\nmc_pending_repository_warning:\n  mc_session_id: {OLD}\n  message: m\n"
            )),
            format!("id: {NEW}\ncwd: /work\nsummary: s\n")
        );
    }

    #[test]
    fn copilot_copy_keeps_the_blank_line_after_an_mc_block() {
        // A blank line ends the block and stays; the top-level key after it stays.
        assert_eq!(
            copied_workspace(&format!(
                "cwd: /work\nmc_pending_repository_warning:\n  mc_session_id: {OLD}\n  message: m\n\nsummary: s\n"
            )),
            format!("id: {NEW}\ncwd: /work\n\nsummary: s\n")
        );
    }

    #[test]
    fn copilot_copy_keeps_an_indented_mc_line_in_another_block() {
        assert_eq!(
            copied_workspace("cwd: /work\ngit:\n  mc_branch: main\n  root: /work\nsummary: s\n"),
            format!("id: {NEW}\ncwd: /work\ngit:\n  mc_branch: main\n  root: /work\nsummary: s\n")
        );
    }

    #[test]
    fn copilot_copy_drops_a_blank_line_inside_an_mc_block() {
        // The blank line does not end the block: it and the indented line after it leave with the block.
        assert_eq!(
            copied_workspace(&format!(
                "cwd: /work\nremote_steerable: false\nmc_pending_repository_warning:\n  mc_session_id: {OLD}\n\n  message: 'not linked: no repo'\nsummary: s\n"
            )),
            format!("id: {NEW}\ncwd: /work\nremote_steerable: false\nsummary: s\n")
        );
    }

    #[test]
    fn copilot_copy_drops_a_comment_inside_an_mc_block() {
        assert_eq!(
            copied_workspace(&format!(
                "cwd: /work\nremote_steerable: false\nmc_pending_repository_warning:\n  mc_session_id: {OLD}\n# continued warning\n  message: 'not linked: no repo'\nsummary: s\n"
            )),
            format!("id: {NEW}\ncwd: /work\nremote_steerable: false\nsummary: s\n")
        );
    }

    #[test]
    fn copilot_copy_keeps_a_comment_between_two_keys() {
        assert_eq!(
            copied_workspace("cwd: /work\n# a note\nsummary: s\n"),
            format!("id: {NEW}\ncwd: /work\n# a note\nsummary: s\n")
        );
    }

    // Whether the copy of a workspace.yaml with `body` below its id line is refused, leaving no target and no temp.
    fn refused(body: &str) -> bool {
        let s = session();
        std::fs::write(
            s.source.join("workspace.yaml"),
            format!("id: {OLD}\n{body}"),
        )
        .unwrap();
        let copied = copy_session(s.home.path(), OLD, NEW);
        !copied && !s.target.exists() && names(s.source.parent().unwrap()) == [OLD]
    }

    #[test]
    fn copilot_copy_refuses_an_mc_sequence_at_column_0() {
        assert!(refused(
            "cwd: /work\nremote_steerable: false\nmc_x:\n- warning\nsummary: s\n"
        ));
    }

    #[test]
    fn copilot_copy_refuses_an_mc_flow_value_left_open() {
        assert!(refused(
            "cwd: /work\nmc_x: {\n  message: m\n}\nsummary: s\n"
        ));
    }

    #[test]
    fn copilot_copy_refuses_an_anchor_inside_an_mc_block() {
        // Dropping the anchor would leave `*shared` pointing at nothing.
        assert!(refused(
            "cwd: /work\nmc_x:\n  message: &shared value\nsummary: *shared\n"
        ));
    }

    #[test]
    fn copilot_copy_refuses_an_anchor_on_the_mc_key_line() {
        assert!(refused(
            "cwd: /work\nmc_x: &shared value\nsummary: *shared\n"
        ));
    }

    #[test]
    fn copilot_copy_refuses_an_ampersand_in_a_quoted_mc_value() {
        // Not an anchor, but the filter does not parse quotes: refused, and the member starts fresh.
        assert!(refused(
            "cwd: /work\nmc_x:\n  message: 'this & that'\nsummary: s\n"
        ));
    }

    #[test]
    fn copilot_copy_drops_a_one_line_flow_mc_value() {
        assert_eq!(
            copied_workspace("cwd: /work\nmc_x: { a: 1 }\nsummary: s\n"),
            format!("id: {NEW}\ncwd: /work\nsummary: s\n")
        );
    }

    #[test]
    fn copilot_copy_drops_a_plain_mc_value() {
        assert_eq!(
            copied_workspace("cwd: /work\nmc_x: plain\nsummary: s\n"),
            format!("id: {NEW}\ncwd: /work\nsummary: s\n")
        );
    }

    #[test]
    fn without_mc_drops_a_crlf_mc_block_whole() {
        // The filter itself: copy_session refuses a CRLF workspace.yaml at its `id:` line check.
        assert_eq!(
            without_mc(
                "id: x\r\ncwd: /work\r\nmc_pending_repository_warning:\r\n  mc_session_id: x\r\n  message: m\r\nsummary: s\r\n"
            )
            .as_deref(),
            Some("id: x\r\ncwd: /work\r\nsummary: s\r\n")
        );
    }

    #[test]
    fn copilot_copy_rules() {
        // A partial last line is dropped, the id rewritten, mc_ lines leave workspace.yaml only, subfolders stay.
        let s = session();
        let events = s.source.join("events.jsonl");
        let complete = read(&events);
        std::fs::write(
            &events,
            format!("{complete}{{\"type\":\"assistant.message\",\"content\":\"half"),
        )
        .unwrap();
        std::fs::write(
            s.source.join("extra.yaml"),
            format!("mc_keep: 1\nref: {OLD}\ntail: last"),
        )
        .unwrap();
        std::fs::write(s.source.join("keep.jsonl"), "{\"a\":1}\n{\"b\":2}").unwrap();
        std::fs::write(s.source.join("notes.txt"), format!("ref: {OLD}\n")).unwrap();
        std::fs::create_dir_all(s.source.join("checkpoints")).unwrap();
        let nested = format!("{{\"id\":\"{OLD}\"}}\n{{\"half");
        std::fs::write(s.source.join("checkpoints").join("index.jsonl"), &nested).unwrap();
        let before = tree(&s.source);
        assert!(copy_session(s.home.path(), OLD, NEW));
        assert_eq!(
            read(&s.target.join("events.jsonl")),
            complete.replace(OLD, NEW)
        );
        assert_eq!(
            read(&s.target.join("workspace.yaml")),
            format!("id: {NEW}\ncwd: /work\nsummary: s\n")
        );
        assert_eq!(
            read(&s.target.join("extra.yaml")),
            format!("mc_keep: 1\nref: {NEW}\ntail: last")
        );
        // A parsable unterminated last line gets its LF.
        assert_eq!(read(&s.target.join("keep.jsonl")), "{\"a\":1}\n{\"b\":2}\n");
        assert_eq!(read(&s.target.join("notes.txt")), format!("ref: {OLD}\n"));
        assert_eq!(
            read(&s.target.join("checkpoints").join("index.jsonl")),
            nested
        );
        assert_eq!(tree(&s.source), before);
        assert_eq!(names(s.target.parent().unwrap()), [OLD, NEW]);

        // A failed check rolls back: no target, no temp folder.
        let failures: [(&str, Break); 6] = [
            ("bad line", |s| {
                let events = s.source.join("events.jsonl");
                let text = read(&events);
                std::fs::write(&events, format!("{text}{{\"type\":broken}}\n")).unwrap();
            }),
            ("blank line", |s| {
                let events = s.source.join("events.jsonl");
                std::fs::write(&events, read(&events).replace('\n', "\n\n")).unwrap();
            }),
            ("no id line", |s| {
                std::fs::write(s.source.join("workspace.yaml"), "cwd: /work\n").unwrap()
            }),
            ("crlf workspace", |s| {
                let yaml = s.source.join("workspace.yaml");
                std::fs::write(&yaml, read(&yaml).replace('\n', "\r\n")).unwrap();
            }),
            ("events without id", |s| {
                std::fs::write(s.source.join("events.jsonl"), "{\"type\":\"x\"}\n").unwrap()
            }),
            ("missing workspace", |s| {
                std::fs::remove_file(s.source.join("workspace.yaml")).unwrap()
            }),
        ];
        for (name, break_it) in failures {
            let s = session();
            break_it(&s);
            let before = tree(&s.source);
            assert!(!copy_session(s.home.path(), OLD, NEW), "{name}");
            assert_eq!(names(s.target.parent().unwrap()), [OLD], "{name}");
            assert_eq!(tree(&s.source), before, "{name}");
        }

        // An existing target, folder or file, is refused and kept.
        let s = session();
        std::fs::create_dir_all(&s.target).unwrap();
        std::fs::write(s.target.join("keep.txt"), "mine").unwrap();
        assert!(!copy_session(s.home.path(), OLD, NEW));
        assert_eq!(names(&s.target), ["keep.txt"]);
        assert_eq!(names(s.target.parent().unwrap()), [OLD, NEW]);
        let s = session();
        std::fs::write(&s.target, "mine").unwrap();
        assert!(!copy_session(s.home.path(), OLD, NEW));
        assert_eq!(read(&s.target), "mine");

        // Ids that are not GUIDs touch nothing.
        let s = session();
        let before = tree(s.home.path());
        for bad in ["../outside", "", "not-a-guid", &format!("{OLD}\n")] {
            assert!(!copy_session(s.home.path(), bad, NEW), "{bad:?}");
            assert!(!copy_session(s.home.path(), OLD, bad), "{bad:?}");
            assert_eq!(tree(s.home.path()), before, "{bad:?}");
        }

        // A missing session creates nothing, not even session-state.
        let home = TempDir::new();
        assert!(!copy_session(home.path(), OLD, NEW));
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    }

    // Whether any real file below `folder` holds `needle`; links are not followed, so only copies count.
    #[cfg(unix)]
    fn holds(folder: &Path, needle: &[u8]) -> bool {
        std::fs::read_dir(folder).unwrap().any(|entry| {
            let path = entry.unwrap().path();
            let kind = std::fs::symlink_metadata(&path).unwrap().file_type();
            if kind.is_dir() {
                holds(&path, needle)
            } else if kind.is_file() {
                let bytes = std::fs::read(&path).unwrap();
                bytes.windows(needle.len()).any(|w| w == needle)
            } else {
                false
            }
        })
    }

    #[cfg(unix)]
    #[test]
    fn copilot_copy_refuses_links() {
        use std::os::unix::fs::symlink;
        const SECRET: &[u8] = b"outside-secret";
        // A session folder that is itself a link to a well-formed session elsewhere: refused, nothing made.
        let s = session();
        let outside = TempDir::new();
        let real = outside.path().join("session");
        std::fs::rename(&s.source, &real).unwrap();
        std::fs::write(real.join("secret.txt"), SECRET).unwrap();
        symlink(&real, &s.source).unwrap();
        let before = tree(outside.path());
        assert!(!copy_session(s.home.path(), OLD, NEW));
        assert_eq!(names(s.target.parent().unwrap()), [OLD]);
        assert!(!holds(s.home.path(), SECRET));
        assert_eq!(tree(outside.path()), before);

        // A linked file or folder anywhere below the session: refused, the temp folder removed, nothing of the
        // link's target copied.
        type Link = fn(&Path, &Path);
        let links: [(&str, Link); 3] = [
            ("top file", |source, outside| {
                symlink(outside.join("secret.txt"), source.join("notes.txt")).unwrap()
            }),
            ("nested file", |source, outside| {
                std::fs::create_dir(source.join("checkpoints")).unwrap();
                symlink(
                    outside.join("secret.txt"),
                    source.join("checkpoints").join("index.jsonl"),
                )
                .unwrap()
            }),
            ("folder", |source, outside| {
                symlink(outside, source.join("files")).unwrap()
            }),
        ];
        for (name, link) in links {
            let s = session();
            let outside = TempDir::new();
            std::fs::write(outside.path().join("secret.txt"), SECRET).unwrap();
            link(&s.source, outside.path());
            let before = tree(outside.path());
            assert!(!copy_session(s.home.path(), OLD, NEW), "{name}");
            assert_eq!(names(s.target.parent().unwrap()), [OLD], "{name}");
            assert!(!holds(s.target.parent().unwrap(), SECRET), "{name}");
            assert_eq!(tree(outside.path()), before, "{name}");
        }
    }
}
