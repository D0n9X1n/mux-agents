//! Pure parsers of what the kernels and command lines hand back; compiled and tested on every host.

/// The fields a Linux `/proc/<pid>/stat` line gives the process table; `start` is raw ticks (field 22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxStat {
    pub pid: u32,
    pub ppid: u32,
    pub start: i64,
}

// Plain ASCII digits only: no sign, space or empty text.
fn number<T: TryFrom<u64>>(text: &[u8]) -> Option<T> {
    if text.is_empty() || text.len() > 19 || !text.iter().all(u8::is_ascii_digit) {
        return None;
    }
    T::try_from(text.iter().fold(0u64, |n, d| n * 10 + u64::from(d - b'0'))).ok()
}

/// comm may hold spaces, ')' and newlines, so the fields are read after the last ')'. Every field through
/// starttime and the one after it must be there, so a starttime cut short is refused (Unix.ps1:89-94).
pub fn linux_stat(text: &[u8]) -> Option<LinuxStat> {
    let open = text.windows(2).position(|w| w == b" (")?;
    let close = text.iter().rposition(|&b| b == b')')?;
    if open < 1 || close < open {
        return None;
    }
    let rest = std::str::from_utf8(&text[close + 1..]).ok()?.trim();
    let fields: Vec<&str> = rest.split(' ').collect();
    if fields.len() < 21 {
        return None;
    }
    Some(LinuxStat {
        pid: number(&text[..open])?,
        ppid: number(fields[1].as_bytes())?,
        start: number(fields[19].as_bytes())?,
    })
}

/// `/proc/<pid>/cmdline`: it must end in NUL and only that one is dropped, so empty last arguments survive.
pub fn linux_cmdline(bytes: &[u8]) -> Option<Vec<String>> {
    let body = bytes.strip_suffix(b"\0")?;
    Some(
        body.split(|&b| b == 0)
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect(),
    )
}

/// KERN_PROCARGS2: int argc, exec path, NUL padding up to offset 4 + 8k, then argc NUL-terminated strings. The
/// padding must be all NUL and argv[0] non-empty, since an empty one cannot be told from padding (Native.Unix.cs:43-56).
pub fn procargs2(buf: &[u8]) -> Option<Vec<String>> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    if argc < 1 {
        return None;
    }
    let path_end = 4 + buf[4..].iter().position(|&b| b == 0)?;
    let mut at = 4 + ((path_end + 4) & !7);
    if buf[path_end..at.min(buf.len())].iter().any(|&b| b != 0)
        || buf.get(at).is_none_or(|&b| b == 0)
    {
        return None;
    }
    // Grown as strings are found, so a huge argc reserves nothing.
    let mut argv = Vec::new();
    for _ in 0..argc {
        let end = at + buf.get(at..)?.iter().position(|&b| b == 0)?;
        argv.push(String::from_utf8_lossy(&buf[at..end]).into_owned());
        at = end + 1;
    }
    Some(argv)
}

/// `proc_pidinfo(PROC_PIDVNODEPATHINFO)` fills a 2352-byte proc_vnodepathinfo; the cwd path starts at byte 152.
pub const VNODE_INFO_SIZE: usize = 2352;
const VNODE_CWD_AT: usize = 152;
const MAXPATHLEN: usize = 1024;

/// The cwd from proc_vnodepathinfo; a short read, an empty or unterminated path, or one that is not UTF-8 is none.
/// Non-UTF-8 is refused deliberately (fail closed): a lossily decoded path cannot name a real directory, and the
/// directory check would refuse it either way.
pub fn vnode_cwd(info: &[u8], returned: i32) -> Option<String> {
    if returned != VNODE_INFO_SIZE as i32 || info.len() < VNODE_INFO_SIZE {
        return None;
    }
    let path = &info[VNODE_CWD_AT..VNODE_CWD_AT + MAXPATHLEN];
    match path.iter().position(|&b| b == 0)? {
        0 => None,
        end => String::from_utf8(path[..end].to_vec()).ok(),
    }
}

/// A pidfd poll shows exit for one ready entry with POLLIN and neither POLLERR nor POLLNVAL (an fd that is not
/// open); POLLHUP may join POLLIN.
pub fn poll_shows_exit(count: i32, revents: i16) -> bool {
    const POLLIN: i16 = 1;
    const POLLERR: i16 = 8;
    const POLLNVAL: i16 = 32;
    count == 1 && revents & POLLIN != 0 && revents & (POLLERR | POLLNVAL) == 0
}

/// CommandLineToArgvW's split, ported from Wine's (verified against Windows by its conformance tests). An empty
/// line gives none, deliberately (fail closed): every real process has an argv[0], and Windows would answer the
/// caller's own path, which is wrong data for another process.
pub fn windows_argv(line: &str) -> Vec<String> {
    let s: Vec<char> = line.chars().collect();
    let blank = |c: char| c == ' ' || c == '\t';
    let mut argv = Vec::new();
    if s.is_empty() {
        return argv;
    }
    // argv[0]: no escapes; a leading quote runs to the next quote, else it runs to the first blank.
    let mut arg = String::new();
    let mut i = 0;
    if s[0] == '"' {
        i = 1;
        while i < s.len() {
            i += 1;
            if s[i - 1] == '"' {
                break;
            }
            arg.push(s[i - 1]);
        }
    } else {
        while i < s.len() && !blank(s[i]) {
            arg.push(s[i]);
            i += 1;
        }
    }
    argv.push(std::mem::take(&mut arg));
    while i < s.len() && blank(s[i]) {
        i += 1;
    }
    if i == s.len() {
        return argv;
    }
    // The rest: 2n backslashes and a quote give n and a delimiter, 2n+1 give n and a literal quote; inside quotes
    // a run of quotes (the opening one counted) gives one literal quote per three.
    let (mut quotes, mut backslashes) = (0u32, 0usize);
    while i < s.len() {
        let c = s[i];
        if blank(c) && quotes == 0 {
            argv.push(std::mem::take(&mut arg));
            backslashes = 0;
            while i < s.len() && blank(s[i]) {
                i += 1;
            }
            if i == s.len() {
                return argv;
            }
        } else if c == '\\' {
            arg.push(c);
            backslashes += 1;
            i += 1;
        } else if c == '"' {
            for _ in 0..backslashes / 2 {
                arg.pop();
            }
            if backslashes % 2 == 0 {
                quotes += 1;
            } else {
                arg.pop();
                arg.push('"');
            }
            i += 1;
            backslashes = 0;
            while i < s.len() && s[i] == '"' {
                quotes += 1;
                if quotes == 3 {
                    arg.push('"');
                    quotes = 0;
                }
                i += 1;
            }
            if quotes == 2 {
                quotes = 0;
            }
        } else {
            arg.push(c);
            backslashes = 0;
            i += 1;
        }
    }
    argv.push(arg);
    argv
}

/// The UTF-16 length of `args` as ProcessStartInfo.ArgumentList joins them (MuxAgents.ps1:727-744): one space
/// apart; an empty argument or one with whitespace or '"' is quoted, each '"' becomes \" and a backslash run before a
/// '"' or the closing quote doubles.
pub fn cmdline_length(args: &[&str]) -> usize {
    let mut length = args.len().saturating_sub(1);
    for arg in args {
        let mut quoted = arg.is_empty();
        let (mut escapes, mut run) = (0, 0);
        for c in arg.chars() {
            match c {
                '\\' => {
                    run += 1;
                    continue;
                }
                '"' => {
                    quoted = true;
                    escapes += run + 1;
                }
                // char::is_whitespace and .NET's char.IsWhiteSpace are both Unicode White_Space.
                c if c.is_whitespace() => quoted = true,
                _ => {}
            }
            run = 0;
        }
        length += arg.encode_utf16().count() + if quoted { escapes + run + 2 } else { 0 };
    }
    length
}

const NODE_VALUE_OPTIONS: [&str; 4] = ["-r", "--require", "--import", "--loader"];
const NODE_ALLOWED_ASSIGNMENTS: [&str; 10] = [
    "max-old-space-size",
    "max-semi-space-size",
    "stack-size",
    "require",
    "import",
    "loader",
    "experimental-loader",
    "conditions",
    "title",
    "unhandled-rejections",
];
const NODE_ALLOWED_FLAGS: [&str; 7] = [
    "--no-warnings",
    "--no-deprecation",
    "--enable-source-maps",
    "--trace-warnings",
    "--preserve-symlinks",
    "--preserve-symlinks-main",
    "--expose-gc",
];

// --eval, --print (alone or with =value) or a short-option cluster holding e or p.
fn node_evals(item: &str) -> bool {
    if let Some(option) = item.strip_prefix("--") {
        return ["eval", "print"].iter().any(|name| {
            option
                .strip_prefix(name)
                .is_some_and(|r| r.is_empty() || r.starts_with('='))
        });
    }
    item.strip_prefix('-').is_some_and(|cluster| {
        cluster.bytes().all(|b| b.is_ascii_alphabetic()) && cluster.contains(['e', 'p'])
    })
}

/// The script node runs, read fail-closed (Unix.ps1:23-31): the first argument after node that is not an option;
/// `--` ends options; -r/--require/--import/--loader skip their value; only allowlisted options that cannot change
/// what node runs are passed over. Eval, print, any other option or no script gives none.
pub fn node_script(argv: &[String]) -> Option<&str> {
    let mut index = 1;
    while index < argv.len() {
        let item = argv[index].as_str();
        if item == "--" {
            return argv.get(index + 1).map(String::as_str);
        }
        if node_evals(item) {
            return None;
        }
        if NODE_VALUE_OPTIONS.contains(&item) {
            index += 2;
            continue;
        }
        if !item.starts_with('-') {
            return Some(item);
        }
        let allowed = item
            .strip_prefix("--")
            .and_then(|option| option.split_once('='))
            .is_some_and(|(name, _)| NODE_ALLOWED_ASSIGNMENTS.contains(&name))
            || NODE_ALLOWED_FLAGS.contains(&item);
        if !allowed {
            return None;
        }
        index += 1;
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    Claude,
    Copilot,
}

/// A Unix row's client by its real executable path (Unix.ps1:36-47), case-sensitive: claude under
/// `*/claude/versions/*` or named claude; copilot named copilot, or node whose script's realpath ends in
/// `/@github/copilot/npm-loader.js`. A relative script joins the process's cwd, read only then; an unreadable cwd
/// or a path realpath cannot resolve is no client.
pub fn unix_client(
    path: Option<&str>,
    argv: Option<&[String]>,
    cwd: impl FnOnce() -> Option<String>,
    realpath: impl FnOnce(&str) -> Option<String>,
) -> Option<Client> {
    let path = path?;
    let file = path.rsplit('/').next().unwrap_or(path);
    if path.contains("/claude/versions/") || file == "claude" {
        return Some(Client::Claude);
    }
    if file == "copilot" {
        return Some(Client::Copilot);
    }
    if file != "node" {
        return None;
    }
    let script = node_script(argv?).filter(|s| !s.is_empty())?;
    let full = if script.starts_with('/') {
        script.to_string()
    } else {
        let cwd = cwd().filter(|c| c.starts_with('/'))?;
        format!("{}/{script}", cwd.trim_end_matches('/'))
    };
    realpath(&full)?
        .ends_with("/@github/copilot/npm-loader.js")
        .then_some(Client::Copilot)
}

fn windows_file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// A Windows row's client by image name, any case (MuxAgents.ps1:200-204).
pub fn windows_client(path: &str) -> Option<Client> {
    let file = windows_file_name(path);
    if file.eq_ignore_ascii_case("claude.exe") {
        Some(Client::Claude)
    } else if file.eq_ignore_ascii_case("copilot.exe") {
        Some(Client::Copilot)
    } else {
        None
    }
}

/// The shells allowed between a Windows caller and its agent or pane, by image name, any case (MuxAgents.ps1:205).
pub const WINDOWS_SHELLS: [&str; 5] = [
    "pwsh.exe",
    "powershell.exe",
    "cmd.exe",
    "bash.exe",
    "sh.exe",
];

pub fn windows_is_shell(path: &str) -> bool {
    let file = windows_file_name(path);
    WINDOWS_SHELLS
        .iter()
        .any(|shell| file.eq_ignore_ascii_case(shell))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // Unix.Tests.ps1:93.
    fn stat_line(pid: u32, comm: &str, ppid: u32, start: i64) -> String {
        format!(
            "{pid} ({comm}) S {ppid} {pid} {pid} 0 -1 4194560 100 0 0 0 5 3 0 0 20 0 1 0 {start} 1000000 200 0\n"
        )
    }

    #[test]
    fn linux_stat_comm_with_parens_spaces_newlines() {
        let want = LinuxStat {
            pid: 300,
            ppid: 100,
            start: 4321,
        };
        for comm in ["sleep", "a) (b", "a) (b\nc", "x y) 7 8 (", ""] {
            let line = stat_line(300, comm, 100, 4321);
            assert_eq!(linux_stat(line.as_bytes()), Some(want), "{comm:?}");
        }
        // A kernel thread's ppid 2 and a start past i32.
        let big = stat_line(303, "kworker/0:1", 2, 1 << 40);
        assert_eq!(
            linux_stat(big.as_bytes()),
            Some(LinuxStat {
                pid: 303,
                ppid: 2,
                start: 1 << 40
            })
        );
    }

    #[test]
    fn linux_stat_short_refused() {
        let line = stat_line(300, "claude", 100, 4321);
        assert!(
            linux_stat(line.as_bytes()).is_some(),
            "the uncut line parses"
        );
        // Unix.Tests.ps1:105: starttime cut to "43" with nothing after it.
        let cut = &line[..line.find(" 4321 ").unwrap() + 3];
        for bad in [
            cut,
            "x",
            "300 (sh) S 100",
            "",
            "abc (sh) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 77 1",
            "(sh) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 77 1",
            "300 (sh S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 77 1",
            "+300 (sh) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 77 1",
            "300 (sh) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 x7 1",
        ] {
            assert_eq!(linux_stat(bad.as_bytes()), None, "{bad:?}");
        }
    }

    #[test]
    fn linux_cmdline_needs_final_nul_keeps_empty_args() {
        assert_eq!(
            linux_cmdline(b"node\0script.js\0\0"),
            Some(strings(&["node", "script.js", ""]))
        );
        assert_eq!(linux_cmdline(b"a b\0"), Some(strings(&["a b"])));
        assert_eq!(linux_cmdline(b"claude\0--model"), None);
        assert_eq!(linux_cmdline(b""), None);
        assert_eq!(linux_cmdline(b"\0"), Some(strings(&[""])));
    }

    // KERN_PROCARGS2 bytes: int argc, exec path, NUL padding to (4 + 8k), then the strings (Unix.Tests.ps1:7).
    fn procargs(argc: i32, rest: &[u8]) -> Vec<u8> {
        let mut bytes = argc.to_ne_bytes().to_vec();
        bytes.extend_from_slice(rest);
        bytes
    }

    #[test]
    fn procargs2_padding_and_argc() {
        let good = procargs(2, b"/bin/sleep\0\0\0\0\0\0sleep\x0060\0A=1\0");
        assert_eq!(procargs2(&good), Some(strings(&["sleep", "60"])));
        let three = procargs(3, b"/bin/sleep\0\0\0\0\0\0sleep\x0060\0A=1\0");
        assert_eq!(
            procargs2(&three[..33]),
            Some(strings(&["sleep", "60", "A=1"]))
        );
        // Too little padding, text in the padding, a negative argc, a cut buffer.
        for bad in [
            procargs(2, b"/bin/x\0\0\0--forbidden\0A=1\0B=2\0"),
            procargs(2, b"/bin/sleep\0\0\0sleep\x0060\0A=1\0"),
            procargs(-1, b"/bin/sleep\0\0\0\0\0\0sleep\0"),
            procargs(0, b"/bin/sleep\0\0\0\0\0\0sleep\0"),
            good[..28].to_vec(),
            good[..3].to_vec(),
            procargs(1, b"/bin/sleep"),
        ] {
            assert_eq!(procargs2(&bad), None, "{bad:?}");
        }
        // An empty argument after argv[0] is kept.
        let empty_last = procargs(2, b"/bin/x\0\0x\0\0");
        assert_eq!(procargs2(&empty_last), Some(strings(&["x", ""])));
        // A huge argc reads only what is there, without reserving for it.
        assert_eq!(procargs2(&procargs(i32::MAX, b"/bin/x\0\0x\0")), None);
    }

    #[test]
    fn procargs2_empty_argv0_refused() {
        assert_eq!(
            procargs2(&procargs(1, b"/bin/x\0\0x\0")),
            Some(strings(&["x"]))
        );
        // An empty argv[0] cannot be told from padding.
        assert_eq!(procargs2(&procargs(1, b"/bin/x\0\0\0")), None);
        assert_eq!(procargs2(&procargs(2, b"/bin/x\0\0\0y\0")), None);
    }

    #[test]
    fn vnode_cwd_offset_152() {
        let mut info = vec![0u8; VNODE_INFO_SIZE];
        info[152..152 + 14].copy_from_slice(b"/Users/me/work");
        // The root dir path (rdir) later in the struct is not the cwd.
        info[1328..1334].copy_from_slice(b"/Users");
        assert_eq!(vnode_cwd(&info, 2352).as_deref(), Some("/Users/me/work"));
        assert_eq!(vnode_cwd(&info, 2351), None);
        assert_eq!(vnode_cwd(&info[..2000], 2352), None);
        // Empty, unterminated within 1024 bytes, not UTF-8.
        assert_eq!(vnode_cwd(&vec![0u8; VNODE_INFO_SIZE], 2352), None);
        let mut long = vec![0u8; VNODE_INFO_SIZE];
        long[152..152 + 1024].fill(b'a');
        assert_eq!(vnode_cwd(&long, 2352), None);
        // Kept by choice: not UTF-8 is none, not a lossy path with U+FFFD in it (fail closed).
        let mut bad = vec![0u8; VNODE_INFO_SIZE];
        bad[152..155].copy_from_slice(b"/\xff/");
        assert_eq!(vnode_cwd(&bad, 2352), None);
    }

    #[test]
    fn poll_shows_exit_rules() {
        let answers: Vec<bool> = [
            (1, 1),
            (1, 17),
            (0, 0),
            (1, 32),
            (1, 33),
            (1, 9),
            (1, 16),
            (-1, 0),
        ]
        .iter()
        .map(|&(count, revents)| poll_shows_exit(count, revents))
        .collect();
        assert_eq!(
            answers,
            [true, true, false, false, false, false, false, false]
        );
    }

    #[test]
    fn windows_argv_split_matches_commandlinetoargvw() {
        // Wine's conformance table, checked against Windows (dlls/shell32/tests/shlexec.c, cmdline_tests).
        let table: &[(&str, &[&str])] = &[
            ("exe", &["exe"]),
            (
                "exe arg1 arg2 \"arg three\" 'four five` six\\ $even)",
                &[
                    "exe",
                    "arg1",
                    "arg2",
                    "arg three",
                    "'four",
                    "five`",
                    "six\\",
                    "$even)",
                ],
            ),
            (
                "exe arg=1 arg-2 three\tfour\rfour\nfour ",
                &["exe", "arg=1", "arg-2", "three", "four\rfour\nfour"],
            ),
            (
                "exe arg\"one\" \"second\"arg thirdarg ",
                &["exe", "argone", "secondarg", "thirdarg"],
            ),
            ("exe arg1 \"unclosed", &["exe", "arg1", "unclosed"]),
            ("exe arg1 \"", &["exe", "arg1", ""]),
            (
                "exe \"one^\" \"arg\"&two three|four",
                &["exe", "one^", "arg&two", "three|four"],
            ),
            (
                "exe o\\ne t\\\\wo t\\\\\\ree f\\\\\\\\our ",
                &["exe", "o\\ne", "t\\\\wo", "t\\\\\\ree", "f\\\\\\\\our"],
            ),
            (
                "exe \"o\\ne\" \"t\\\\wo\" \"t\\\\\\ree\" \"f\\\\\\\\our\" ",
                &["exe", "o\\ne", "t\\\\wo", "t\\\\\\ree", "f\\\\\\\\our"],
            ),
            (
                "exe \\\"one \\\\\"two\" \\\\\\\"three \\\\\\\\\"four\" end",
                &["exe", "\"one", "\\two", "\\\"three", "\\\\four", "end"],
            ),
            (
                "exe \"one\\\" still\" \"two\\\\\" \"three\\\\\\\" still\" \"four\\\\\\\\\" end",
                &[
                    "exe",
                    "one\" still",
                    "two\\",
                    "three\\\" still",
                    "four\\\\",
                    "end",
                ],
            ),
            ("exe two\"\"quotes next", &["exe", "twoquotes", "next"]),
            (
                "exe three\"\"\"quotes next",
                &["exe", "three\"quotes", "next"],
            ),
            (
                "exe four\"\"\"\" quotes\" next 4%3=1",
                &["exe", "four\" quotes", "next", "4%3=1"],
            ),
            (
                "exe five\"\"\"\"\"quotes next",
                &["exe", "five\"quotes", "next"],
            ),
            (
                "exe six\"\"\"\"\"\"quotes next",
                &["exe", "six\"\"quotes", "next"],
            ),
            (
                "exe seven\"\"\"\"\"\"\" quotes\" next 7%3=1",
                &["exe", "seven\"\" quotes", "next", "7%3=1"],
            ),
            (
                "exe twelve\"\"\"\"\"\"\"\"\"\"\"\"quotes next",
                &["exe", "twelve\"\"\"\"quotes", "next"],
            ),
            (
                "exe thirteen\"\"\"\"\"\"\"\"\"\"\"\"\" quotes\" next 13%3=1",
                &["exe", "thirteen\"\"\"\" quotes", "next", "13%3=1"],
            ),
            ("exe \"two\"\"quotes next", &["exe", "two\"quotes", "next"]),
            ("exe \"two\"\" next", &["exe", "two\"", "next"]),
            (
                "exe \"three\"\"\" quotes\" next 4%3=1",
                &["exe", "three\" quotes", "next", "4%3=1"],
            ),
            (
                "exe \"four\"\"\"\"quotes next",
                &["exe", "four\"quotes", "next"],
            ),
            (
                "exe \"five\"\"\"\"\"quotes next",
                &["exe", "five\"\"quotes", "next"],
            ),
            (
                "exe \"six\"\"\"\"\"\" quotes\" next 7%3=1",
                &["exe", "six\"\" quotes", "next", "7%3=1"],
            ),
            (
                "exe \"eleven\"\"\"\"\"\"\"\"\"\"\"quotes next",
                &["exe", "eleven\"\"\"\"quotes", "next"],
            ),
            (
                "exe \"twelve\"\"\"\"\"\"\"\"\"\"\"\" quotes\" next 13%3=1",
                &["exe", "twelve\"\"\"\" quotes", "next", "13%3=1"],
            ),
            (
                "exe \"the crazy \\\\\"\"\"\\\\\" quotes",
                &["exe", "the crazy \\\"\\", "quotes"],
            ),
            // argv[0] rules: no backslash escapes, a leading quote ends at the next quote, else at the first blank.
            ("exe\"file\"path arg1", &["exe\"file\"path", "arg1"]),
            ("exe\"file\"path\targ1", &["exe\"file\"path", "arg1"]),
            ("exe\"path\\ arg1", &["exe\"path\\", "arg1"]),
            ("\\\"exe \"arg one\"", &["\\\"exe", "arg one"]),
            ("\"spaced exe\" \"next arg\"", &["spaced exe", "next arg"]),
            ("\"spaced exe\"\t\"next arg\"", &["spaced exe", "next arg"]),
            ("\"exe\"arg\" one\" argtwo", &["exe", "arg one", "argtwo"]),
            (
                "\"spaced exe\\\"arg1 arg2",
                &["spaced exe\\", "arg1", "arg2"],
            ),
            ("\"two\"\" arg1 ", &["two", " arg1 "]),
            ("\"three\"\"\" arg2", &["three", "", "arg2"]),
            ("\"four\"\"\"\"arg1", &["four", "\"arg1"]),
            (" \"arg\"one argtwo", &["", "argone", "argtwo"]),
            ("\"unclosed exe", &["unclosed exe"]),
            ("exe   ", &["exe"]),
            ("中文.exe 中 文", &["中文.exe", "中", "文"]),
        ];
        for (line, want) in table {
            assert_eq!(windows_argv(line), strings(want), "{line:?}");
        }
        // Kept by choice: an empty line is no argv at all, read as unreadable (fail closed). A real process always
        // has an argv[0], and CommandLineToArgvW would answer the CALLER's own exe path, which is wrong data for
        // another process.
        assert_eq!(windows_argv(""), Vec::<String>::new());
    }

    // ProcessStartInfo.ArgumentList's join (PasteArguments.AppendArgument): quote an empty argument or one with
    // whitespace or '"'; a '"' becomes \" and a backslash run before it or the closing quote doubles.
    fn argument_list_join(args: &[&str]) -> String {
        let mut line = String::new();
        for (index, arg) in args.iter().enumerate() {
            if index > 0 {
                line.push(' ');
            }
            if !arg.is_empty() && !arg.chars().any(|c| c.is_whitespace() || c == '"') {
                line.push_str(arg);
                continue;
            }
            line.push('"');
            let mut run = 0;
            for c in arg.chars() {
                match c {
                    '\\' => run += 1,
                    '"' => {
                        line.push_str(&"\\".repeat(run * 2 + 1));
                        run = 0;
                    }
                    _ => {
                        line.push_str(&"\\".repeat(run));
                        run = 0;
                    }
                }
                if c != '\\' {
                    line.push(c);
                }
            }
            line.push_str(&"\\".repeat(run * 2));
            line.push('"');
        }
        line
    }

    #[test]
    fn cmdline_length_matches_argumentlist_join() {
        // MuxAgents.Tests.ps1:2378.
        for (arg, want) in [
            ("", 2),
            ("x", 1),
            ("\"", 4),
            ("\\\"", 6),
            ("\\\\\"", 8),
            ("a\\b", 3),
            ("a b\\", 7),
            ("a b\\\\", 9),
        ] {
            assert_eq!(cmdline_length(&[arg]), want, "{arg:?}");
        }
        assert_eq!(cmdline_length(&["a", "b"]), 3);
        assert_eq!(cmdline_length(&[]), 0);
        let args = [
            "--model",
            "a b",
            "",
            "run\\\\\\\"x",
            "\\\\a\\\\",
            "ideo\u{3000}space",
            "中文 中",
            "tab\there",
            "end\\",
            "\u{1F600} wide",
        ];
        let joined = argument_list_join(&args);
        // Counted in UTF-16 units, like the 32767-character limit.
        assert_eq!(cmdline_length(&args), joined.encode_utf16().count());
        // The join is what CommandLineToArgvW splits back into the same arguments.
        let split = windows_argv(&format!("exe {joined}"));
        assert_eq!(split[1..], strings(&args)[..]);
    }

    #[test]
    fn node_script_rules() {
        let loader = "/l/@github/copilot/npm-loader.js";
        let script = |items: &[&str]| node_script(&strings(items)).map(str::to_string);
        let some = |s: &str| Some(s.to_string());
        assert_eq!(script(&["node", loader, "--allow-all"]), some(loader));
        assert_eq!(script(&["node", "--no-warnings", loader]), some(loader));
        assert_eq!(
            script(&["node", "-r", "hook.js", "--import", "x.mjs", "--", loader]),
            some(loader)
        );
        assert_eq!(
            script(&["node", "--require", "hook.js", loader]),
            some(loader)
        );
        assert_eq!(
            script(&["node", "--loader", "hook.mjs", loader]),
            some(loader)
        );
        assert_eq!(
            script(&["node", "--require", loader, "/srv/app.js"]),
            some("/srv/app.js")
        );
        assert_eq!(
            script(&[
                "node",
                "--max-old-space-size=4096",
                "--enable-source-maps",
                "--no-deprecation",
                loader
            ]),
            some(loader)
        );
        assert_eq!(script(&["node", "--", "-dash.js"]), some("-dash.js"));
        // Eval and print, alone or in a short cluster: whatever follows is not what node runs.
        for eval in [
            "-e",
            "-p",
            "--eval",
            "--print",
            "--eval=1",
            "--print=1",
            "-pe",
            "-ie",
            "-xp",
        ] {
            assert_eq!(script(&["node", eval, "1", loader]), None, "{eval}");
        }
        // Any other option may take the next argument or run something else.
        for other in [
            "--conditions",
            "-C",
            "--some-new-flag",
            "--run=probe",
            "--input-type=module",
            "--test",
            "--check",
        ] {
            assert_eq!(script(&["node", other, loader]), None, "{other}");
        }
        assert_eq!(script(&["node"]), None);
        assert_eq!(script(&["node", "--"]), None);
        assert_eq!(script(&["node", "-r"]), None);
        assert_eq!(script(&["node", "--no-warnings"]), None);
    }

    #[test]
    fn node_option_allowlists_are_the_reference_lists() {
        // Unix.ps1:31, verbatim: these and no others are passed over.
        let assignments = [
            "max-old-space-size",
            "max-semi-space-size",
            "stack-size",
            "require",
            "import",
            "loader",
            "experimental-loader",
            "conditions",
            "title",
            "unhandled-rejections",
        ];
        let flags = [
            "--no-warnings",
            "--no-deprecation",
            "--enable-source-maps",
            "--trace-warnings",
            "--preserve-symlinks",
            "--preserve-symlinks-main",
            "--expose-gc",
        ];
        assert_eq!(NODE_ALLOWED_ASSIGNMENTS, assignments);
        assert_eq!(NODE_ALLOWED_FLAGS, flags);
        let loader = "/l/@github/copilot/npm-loader.js";
        let script =
            |option: &str| node_script(&strings(&["node", option, loader])).map(str::to_string);
        for name in assignments {
            let option = format!("--{name}=1");
            assert_eq!(script(&option).as_deref(), Some(loader), "{option}");
            // Only as --name=value; alone it may take the next argument.
            assert_eq!(script(&format!("--{name}")), None, "--{name}");
        }
        for flag in flags {
            assert_eq!(script(flag).as_deref(), Some(loader), "{flag}");
            assert_eq!(script(&format!("{flag}=1")), None, "{flag}=1");
        }
        for unlisted in [
            "--unrequested=1",
            "--unrequested-flag",
            "--Title=x",
            "--no-warning",
        ] {
            assert_eq!(script(unlisted), None, "{unlisted}");
        }
    }

    #[test]
    fn unix_client_by_path() {
        // Unix.Tests.ps1:44-74: realpath follows these links, else answers the path; pid 401's cwd is /p, 402's /q.
        let loader = "/usr/local/lib/node_modules/@github/copilot/npm-loader.js";
        let realpath = |path: &str| match path {
            "/broken" => None,
            "/usr/local/bin/copilot" | "bin/copilot" => Some(loader.to_string()),
            "/p/bin/copilot" => Some("/p/node_modules/@github/copilot/npm-loader.js".to_string()),
            // Spelled as the loader, a link to an ordinary app: the canonical path decides.
            "/x/@github/copilot/npm-loader.js" => Some("/srv/app.js".to_string()),
            other => Some(other.to_string()),
        };
        let client = |path: Option<&str>, argv: &[&str], cwd: Option<&str>| {
            let argv = strings(argv);
            unix_client(path, Some(&argv), || cwd.map(str::to_string), realpath)
        };
        let node = Some("/usr/bin/node");
        use Client::{Claude, Copilot};
        let named = [
            client(
                Some("/home/u/.local/share/claude/versions/2.1.3"),
                &["claude"],
                None,
            ),
            client(Some("/opt/claude"), &["x"], None),
            client(Some("/usr/bin/copilot"), &["copilot"], None),
            client(
                node,
                &["node", "/usr/local/bin/copilot", "--allow-all"],
                None,
            ),
            client(node, &["node", loader], None),
            client(node, &["node", "--no-warnings", loader], None),
            client(
                node,
                &["node", "-r", "hook.js", "--import", "x.mjs", "--", loader],
                None,
            ),
            client(node, &["node", "bin/copilot"], Some("/p")),
            client(node, &["node", "bin/copilot"], Some("/p/")),
        ];
        assert_eq!(
            named,
            [
                Claude, Claude, Copilot, Copilot, Copilot, Copilot, Copilot, Copilot, Copilot
            ]
            .map(Some)
        );
        let none = [
            client(Some("/usr/bin/sleep"), &["claude"], None),
            client(Some("/usr/bin/bash"), &["copilot", "--allow-all"], None),
            client(Some("/usr/bin/claude.exe"), &["claude"], None),
            client(Some("/opt/Claude"), &["claude"], None),
            client(Some("/x/claude/versionsX/1"), &["claude"], None),
            client(Some("/usr/bin/copilot-cli"), &["copilot"], None),
            client(node, &["node", "/srv/app.js"], None),
            client(
                node,
                &["node", "/x/@github/copilot/npm-loader.js.bak"],
                None,
            ),
            client(
                Some("/usr/bin/bun"),
                &["bun", "/usr/local/bin/copilot"],
                None,
            ),
            client(node, &["node"], None),
            client(node, &["node", "/broken"], None),
            client(None, &["claude"], None),
            client(node, &["node", "-e", "require(1)", loader], None),
            client(node, &["node", "-p", "1", loader], None),
            client(node, &["node", "--eval=1", loader], None),
            client(node, &["node", "-r", loader, "/srv/app.js"], None),
            client(node, &["node", "bin/copilot"], Some("/q")),
            client(node, &["node", "bin/copilot"], None),
            client(node, &["node", "bin/copilot"], Some("relative")),
            client(node, &["node", ""], Some("/p")),
            client(node, &["node", "/x/@github/copilot/npm-loader.js"], None),
        ];
        assert_eq!(none, [None; 21]);
        // An unreadable argv is no node client; the cwd is read only for a relative script.
        assert_eq!(unix_client(node, None, || None, realpath), None);
        let read = std::cell::Cell::new(false);
        let argv = strings(&["node", loader]);
        let cwd = || {
            read.set(true);
            None
        };
        assert_eq!(unix_client(node, Some(&argv), cwd, realpath), Some(Copilot));
        assert!(!read.get());
    }

    #[test]
    fn windows_client_and_shell_names() {
        // Providers.Tests.ps1:116-168.
        let clients: Vec<_> = [
            "/u/claude.exe",
            "/x/Copilot.EXE",
            "C:\\Tools\\CLAUDE.exe",
            "/x/claude",
            "/x/claude.exe.bak",
            "/x/node.exe",
            "",
        ]
        .iter()
        .map(|p| windows_client(p))
        .collect();
        assert_eq!(
            clients,
            [
                Some(Client::Claude),
                Some(Client::Copilot),
                Some(Client::Claude),
                None,
                None,
                None,
                None
            ]
        );
        let shells: Vec<_> = [
            "pwsh.exe",
            "PowerShell.exe",
            "cmd.exe",
            "bash.exe",
            "sh.exe",
            "fish",
            "pwsh",
            "node.exe",
        ]
        .iter()
        .map(|n| windows_is_shell(&format!("/bin/{n}")))
        .collect();
        assert_eq!(shells, [true, true, true, true, true, false, false, false]);
        assert!(windows_is_shell("C:\\Windows\\System32\\cmd.exe"));
    }
}
