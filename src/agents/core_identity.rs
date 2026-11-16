use super::registry::{Launch, Pane, Row};
use crate::caller::Caller;
use crate::hc::Terminal;
use crate::state::root::{HOST, OsKind};
use serde_json::Value;
pub fn generation(g: &str) -> bool {
    !g.is_empty()
        && g.len() <= 64
        && g.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
}
/// launch-1 G2: `m` and 8 lowercase hex digits, the shape of a generation-0 name (`m` and the first 8 of the id)
/// and of each new generation name (`m` and 8 random hex digits, A3 Task 9).
pub fn is_hc_name(name: &str) -> bool {
    name.len() == 9
        && name.strip_prefix("m").is_some_and(|hex| {
            hex.bytes()
                .all(|b| b.is_ascii_digit() || b"abcdef".contains(&b))
        })
}
/// views-6f V1: the presentation key a view was made under: the multiplexer server and session that show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub server: String,
    pub session: String,
}

impl Key {
    pub fn of(caller: &Caller) -> Key {
        Key {
            server: caller.server_key.clone(),
            session: caller.session.clone(),
        }
    }
}

/// v6f calls 6, 14, 18: the program of a pane's start command when it is exactly `<program> attach <address>`
/// (`attachment.rs`), else None. A whitelist recognizer, not a shell parser: tokens are separated by single ASCII
/// spaces; an unquoted program is drawn from `[A-Za-z0-9_./:+-]` (and `\` on Windows) and does not start with `-`; a
/// quoted one (`"`, and `'` on Unix) may also hold spaces; the address is exactly one unquoted token from
/// `[A-Za-z0-9_.:-]`, then the end. The program's file name is an hc install name (`hc::INSTALL_NAMES`), at any path
/// (case-insensitive and without `.exe` on Windows). Everything else is "not an attach".
pub fn attach_program(command: &str, address: &str, os: OsKind) -> Option<String> {
    let unix = os == OsKind::Unix;
    let path = |c: char| c.is_ascii_alphanumeric() || "_./:+-".contains(c) || (!unix && c == '\\');
    let token = |c: char| c.is_ascii_alphanumeric() || "_.:-".contains(c);
    if address.is_empty() || !address.chars().all(token) {
        return None;
    }
    let quotes: &[char] = if unix { &['"', '\''] } else { &['"'] };
    // #150: rmux renders the whole argv in one pair of double quotes, `\` as `\\` and `"` as `\"` inside. Its inside,
    // unescaped and holding no quote, is read as the bare form; any other backslash, or anything else, falls through
    // to the forms below, which refuse it.
    if let Some(inner) = command
        .strip_prefix('"')
        .and_then(|c| c.strip_suffix('"'))
        .and_then(rmux_unquoted)
        .filter(|i| !i.is_empty() && !i.contains(quotes))
    {
        return attach_program(&inner, address, os);
    }
    let first = command.chars().next()?;
    let (program, tail) = if quotes.contains(&first) {
        let end = 1 + command[1..].find(first)?;
        let program = &command[1..end];
        if !program.chars().all(|c| path(c) || c == ' ') {
            return None;
        }
        (program, &command[end + 1..])
    } else {
        let at = command.find(' ')?;
        let program = &command[..at];
        if !program.chars().all(path) {
            return None;
        }
        (program, &command[at..])
    };
    if program.is_empty()
        || program.starts_with('-')
        || tail.strip_prefix(" attach ") != Some(address)
    {
        return None;
    }
    let file = program.rsplit(['/', '\\']).next()?;
    let file = if unix {
        file.to_string()
    } else {
        let lower = file.to_ascii_lowercase();
        lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
    };
    crate::hc::INSTALL_NAMES
        .contains(&file.as_str())
        .then(|| program.to_string())
}

/// The inside of rmux's quoted start command with its two escapes undone (`\\` is `\`, `\"` is `"`); any other
/// backslash is not rmux's form: none.
fn rmux_unquoted(inner: &str) -> Option<String> {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next()? {
                e @ ('\\' | '"') => out.push(e),
                _ => return None,
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// V2: what a holder of the member lock found: the view's pane, whether it was adopted (no valid descriptor), and
/// how many other live panes of the caller's session attach the same address.
#[derive(Debug)]
pub struct Located {
    pub pane: Pane,
    pub adopted: bool,
    pub duplicates: usize,
}

/// `%12` -> 12; anything else sorts last.
fn pane_number(pane: &str) -> u64 {
    pane.strip_prefix('%')
        .and_then(|n| n.parse().ok())
        .unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub address: String,
    pub generation: String,
    /// The hc name of the generation (G2): hcName; for generation 0 without one, m and the first 8 of the id.
    pub hc_name: String,
    pub team: String,
    pub session_id: String,
    pub launcher: Vec<String>,
}
impl Identity {
    pub fn read(r: &Value, id: &str, team: &str) -> Option<Self> {
        let s = |k: &str| r.get(k)?.as_str();
        if !super::agent_id(id)
            || s("id")? != id
            || s("team")? != team
            || s("session")? != team.rsplit_once(':')?.1
            || !super::agent_name(s("name")?)
            || !generation(s("generation")?)
            || !crate::checks::session_id(s("sessionId")?)
            || s("hcAddress")?.is_empty()
        {
            return None;
        }
        let launcher = r
            .get("launcherArgv")?
            .as_array()?
            .iter()
            .map(|v| v.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        if launcher.is_empty() || launcher[0].is_empty() {
            return None;
        }
        Some(Self {
            id: id.into(),
            name: s("name")?.into(),
            address: s("hcAddress")?.into(),
            generation: s("generation")?.into(),
            hc_name: match (r.get("hcName"), s("generation")?) {
                // G2: only an existing generation-0 terminal keeps the derived name.
                (None, "0") => format!("m{}", &id[..8]),
                (None, _) => return None,
                (Some(v), _) => v.as_str().filter(|n| is_hc_name(n))?.to_owned(),
            },
            team: team.into(),
            session_id: s("sessionId")?.into(),
            launcher,
        })
    }
    /// A resume's successor that claimed its run (gen 0) and kept only its pre-run record: hc run's reply, and so
    /// its address and launcher, were lost. Its address is empty until hc's labels name its terminal; with no
    /// launcher it is never Running, so its life is Ended, Lost or Unknown, never "no member".
    pub fn pending(r: &Value, id: &str, team: &str) -> Option<Self> {
        let s = |k: &str| r.get(k)?.as_str();
        if !super::agent_id(id)
            || s("id")? != id
            || s("session")? != team.rsplit_once(':')?.1
            || !super::agent_name(s("name")?)
            || !crate::checks::session_id(s("sessionId")?)
            || !super::agent_id(s("resumedFrom")?)
            || r.get("hcAddress").is_some()
        {
            return None;
        }
        Some(Self {
            id: id.into(),
            name: s("name")?.into(),
            address: String::new(),
            generation: "0".into(),
            hc_name: format!("m{}", &id[..8]),
            team: team.into(),
            session_id: s("sessionId")?.into(),
            launcher: Vec::new(),
        })
    }
    /// R4 (ambiguity 4): the identity a held pending generation stands for; every generation field is the pending
    /// block's. Its address is `pending.hcAddress`, or empty until hc's labels name it; no launcher, never Running.
    pub fn held(r: &Value, id: &str, team: &str) -> Option<Self> {
        let s = |k: &str| r.get(k)?.as_str();
        let p = r.get("pending")?;
        let ps = |k: &str| p.get(k)?.as_str();
        if !super::agent_id(id)
            || s("id")? != id
            || s("session")? != team.rsplit_once(':')?.1
            || !super::agent_name(s("name")?)
            || !crate::checks::session_id(ps("sessionId")?)
            || !is_hc_name(ps("hcName")?)
        {
            return None;
        }
        Some(Self {
            id: id.into(),
            name: s("name")?.into(),
            address: ps("hcAddress").unwrap_or("").into(),
            generation: p.get("gen")?.as_u64()?.to_string(),
            hc_name: ps("hcName")?.into(),
            team: team.into(),
            session_id: ps("sessionId")?.into(),
            launcher: Vec::new(),
        })
    }
    /// a3c call 47: the address matches only when both are valid and equal; a held or pending identity's empty address
    /// never matches.
    pub fn matches(&self, t: &Terminal) -> bool {
        use super::ident;
        ident::same(ident::hc_address, Some(&t.address), Some(&self.address))
            && [
                ("mux.managed", "1"),
                ("mux.team", self.team.as_str()),
                ("mux.id", self.id.as_str()),
                ("mux.gen", self.generation.as_str()),
            ]
            .iter()
            .all(|(k, v)| t.labels.get(*k).map(String::as_str) == Some(*v))
    }
    /// V1, the one resolver: the pane the descriptor names, live, running exactly the recorded start command. A
    /// lifecycle descriptor (gen, hcAddress, hcName already the subject generation's: `presented`) also needs the
    /// caller's presentation key, its own window, and a start command that is exactly `<program> attach <address>`
    /// with the program the descriptor recorded (v6f calls 6, 13, 14, 18): never re-resolved through `PATH`. A
    /// pre-lifecycle record keeps main's rule (ambiguity 3).
    pub fn view(&self, r: &Value, rows: &[Row], key: &Key) -> Option<Pane> {
        let pane = r.get("pane")?.as_str()?;
        let command = r.get("startCommand")?.as_str()?;
        if command.is_empty() {
            return None;
        }
        let row = Self::live(rows, pane)?;
        if row.command != command {
            return None;
        }
        if super::gens::lifecycle(r) {
            let text = |k: &str| r.get(k).and_then(Value::as_str);
            let program = attach_program(command, &self.address, HOST)?;
            // v6f call 24: identity is key + pane + start command + attach program; the window is where the pane is
            // now (break-pane moves it), reported from the row, never matched.
            if text("viewServerKey") != Some(key.server.as_str())
                || text("viewSession") != Some(key.session.as_str())
                || text("viewAttachProgram") != Some(program.as_str())
            {
                return None;
            }
        }
        Some(self.pane_of(row, r))
    }

    /// V2, ambiguity 6, only under the member lock (6f-R2): the live panes `attach_program` recognises for this
    /// address (an hc install name at any path, v6f call 13; never what `PATH` resolves today) are the candidates
    /// (generation n's attaches n's, so never). A valid descriptor keeps its pane; else the lowest numeric id is
    /// adopted. Every other candidate is a duplicate, counted and left alone.
    pub fn search(&self, r: &Value, rows: &[Row], key: &Key) -> Option<Located> {
        let mut candidates: Vec<&Row> = rows
            .iter()
            .filter(|row| {
                attach_program(&row.command, &self.address, HOST).is_some()
                    && Self::live(rows, &row.pane).is_some()
            })
            .collect();
        candidates.sort_by_key(|row| pane_number(&row.pane));
        if let Some(pane) = self.view(r, rows, key) {
            let duplicates = candidates
                .iter()
                .filter(|row| row.pane != pane.pane)
                .count();
            return Some(Located {
                pane,
                adopted: false,
                duplicates,
            });
        }
        let first = candidates.first()?;
        Some(Located {
            pane: self.pane_of(first, r),
            adopted: true,
            duplicates: candidates.len() - 1,
        })
    }

    /// Main's liveness: exactly one row at `pane`, not dead, a pid of at least 2, an `@` window.
    fn live<'r>(rows: &'r [Row], pane: &str) -> Option<&'r Row> {
        let mut found = rows.iter().filter(|row| row.pane == pane);
        let row = found.next()?;
        (found.next().is_none()
            && !row.dead
            && row.pid.parse::<u32>().is_ok_and(|p| p >= 2)
            && row.window.starts_with('@'))
        .then_some(row)
    }

    fn pane_of(&self, row: &Row, r: &Value) -> Pane {
        Pane {
            pane: row.pane.clone(),
            window: row.window.clone(),
            pid: row.pid.clone(),
            id: self.id.clone(),
            name: self.name.clone(),
            record: r.clone(),
            launch: Launch {
                command: row.command.clone(),
                pid: None,
            },
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    pub(crate) fn record() -> Value {
        json!({"id":"aaaaaaaaaaaa","name":"worker","session":"$3","sessionId":"0f11fc2f-2032-48ef-a920-4af2a8e5af50","team":"k:$3","generation":"0","hcAddress":"t1","launcherArgv":["/exe","launcher","/spec"],"pane":"%9","window":"@5","startCommand":"hc attach t1"})
    }
    pub(crate) fn identity() -> Identity {
        Identity::read(&record(), "aaaaaaaaaaaa", "k:$3").unwrap()
    }
    /// `record()` as a lifecycle record whose view `presented` overlaid under key `k`/`$3`.
    fn life_record() -> Value {
        let mut r = record();
        r["lifecycle"] = json!(1);
        r["viewServerKey"] = json!("k");
        r["viewSession"] = json!("$3");
        r["viewAttachProgram"] = json!("hc");
        r
    }
    /// The caller's key (`record()`'s `startCommand` `hc attach t1` runs the recorded program `hc`).
    fn key() -> Key {
        Key {
            server: "k".into(),
            session: "$3".into(),
        }
    }
    fn row9() -> Row {
        Row {
            pane: "%9".into(),
            window: "@5".into(),
            pid: "300".into(),
            dead: false,
            command: "hc attach t1".into(),
            tag: String::new(),
            name: String::new(),
        }
    }

    // V1: the key, the window and the attached address are each needed; K1's %9 reused on K2 is no view on K2.
    #[test]
    fn a_lifecycle_view_needs_its_key_window_and_attach() {
        let id = identity();
        let r = life_record();
        assert_eq!(
            id.view(&r, &[row9()], &key()).map(|p| p.pane),
            Some("%9".to_string()),
            "the whole descriptor"
        );
        for (case, field, value) in [
            ("server", "viewServerKey", json!("k2")),
            ("session", "viewSession", json!("$4")),
            ("no key", "viewServerKey", Value::Null),
        ] {
            let mut bad = r.clone();
            if value.is_null() {
                bad.as_object_mut().unwrap().remove(field);
            } else {
                bad[field] = value;
            }
            assert!(id.view(&bad, &[row9()], &key()).is_none(), "{case}");
        }
        // v6f call 24: the window is not identity. A view moved by break-pane (descriptor @6, pane now in @5) is still
        // the view, reported where the pane actually is.
        let mut moved = r.clone();
        moved["window"] = json!("@6");
        assert_eq!(
            id.view(&moved, &[row9()], &key())
                .map(|p| (p.pane, p.window)),
            Some(("%9".to_string(), "@5".to_string())),
            "a moved window"
        );
        let mut other = r.clone();
        other["startCommand"] = json!("hc attach t2");
        let mut row = row9();
        row.command = "hc attach t2".into();
        assert!(
            id.view(&other, &[row], &key()).is_none(),
            "attach: the row matches but attaches another address"
        );
        let mut other_program = r.clone();
        other_program["viewAttachProgram"] = json!("/usr/bin/herdr-core");
        assert!(
            id.view(&other_program, &[row9()], &key()).is_none(),
            "the recorded program: the pane runs another"
        );
        let k2 = Key {
            server: "k2".into(),
            ..key()
        };
        assert!(
            id.view(&r, &[row9()], &k2).is_none(),
            "server: K1's %9 on K2"
        );
    }

    // v6f calls 6, 14, 18: a whitelist recognizer: `<program> attach <address>`, ASCII spaces only; an unquoted
    // program from `[A-Za-z0-9_./:+-]` (and `\` on Windows) not starting with `-`; a quoted one (platform quotes) adds
    // the space; the address one unquoted token from `[A-Za-z0-9_.:-]`, then the end. The program's file name is an hc
    // install name (`hc`, `herdr-core`), any path. Everything else is "not an attach".
    // #150 live (rmux): rmux 0.10.0 renders `#{pane_start_command}` as the whole argv in one pair of double quotes,
    // `\` written `\\` and `"` written `\"` inside (rmux-server lifecycle_state.rs `quote_single_shell_command`), where
    // tmux renders the argv bare. That one quoted token is an attach exactly when its inside, unescaped, holds no quote
    // and is the bare form. Any other backslash is not rmux's form; a space-holding program is not one (fail closed).
    #[test]
    fn attach_program_reads_a_wholly_quoted_attach() {
        use crate::state::root::OsKind::{Unix, Windows};
        let some = |p: &str| Some(p.to_string());
        for (os, command, want) in [
            (Unix, r#""/x/herdr-core attach t1""#, some("/x/herdr-core")),
            (Unix, r#""hc attach t1""#, some("hc")),
            (
                Windows,
                r#""C:\\hc\\herdr-core.exe attach t1""#,
                some(r"C:\hc\herdr-core.exe"),
            ),
            (Windows, r#""C:\hc\herdr-core.exe attach t1""#, None),
            (Windows, r#""C:\\hc\\herdr-core.exe attach t1\""#, None),
            (Windows, r#""C:\\hc\"x\\hc.exe attach t1""#, None),
            (Unix, r#""/opt/my hc/hc attach t1""#, None),
            (Unix, r#""hc attach t1" x"#, None),
            (Unix, r#""hc attach "t1""#, None),
            (Unix, r#""'hc' attach t1""#, None),
            (Unix, r#""hc attach t2""#, None),
            (Unix, r#""/x/other attach t1""#, None),
            (Unix, r#""""#, None),
            (Windows, r#""C:\Program Files\hc\hc.exe attach t1""#, None),
        ] {
            assert_eq!(
                attach_program(command, "t1", os),
                want,
                "{os:?} {command:?}"
            );
        }
    }

    #[test]
    fn attach_program_recognizes_only_an_hc_attach() {
        use crate::state::root::OsKind::{Unix, Windows};
        let some = |p: &str| Some(p.to_string());
        for (os, command, want) in [
            (Unix, "hc attach t1", some("hc")),
            (
                Unix,
                "/usr/bin/herdr-core attach t1",
                some("/usr/bin/herdr-core"),
            ),
            (Unix, r#""/opt/my hc/hc" attach t1"#, some("/opt/my hc/hc")),
            (Unix, "'/opt/my hc/hc' attach t1", some("/opt/my hc/hc")),
            (
                Windows,
                r#""C:\Program Files\hc\hc.exe" attach t1"#,
                some(r"C:\Program Files\hc\hc.exe"),
            ),
            (
                Windows,
                r"C:\hc\HERDR-CORE.EXE attach t1",
                some(r"C:\hc\HERDR-CORE.EXE"),
            ),
            (Unix, "sh -c 'sleep 3600' attach t1", None),
            (Unix, "/usr/bin/other attach t1", None),
            (Unix, "hc\nattach t1", None),
            (Unix, "hc\u{a0}attach t1", None),
            (Windows, "hc\u{a0}attach t1", None),
            (Unix, "hc attach t1; x", None),
            (Unix, "/x;/hc attach t1", None),
            (Unix, "/x\u{7}/hc attach t1", None),
            (Unix, "/x\u{a0}/hc attach t1", None),
            (Unix, r"/x\/hc attach t1", None),
            (Windows, "'/opt/hc' attach t1", None),
            (Unix, "#/hc attach t1", None),
            (Unix, "(/hc attach t1", None),
            (Unix, "-/hc attach t1", None),
            (Unix, r#""/x;/hc" attach t1"#, None),
            (Windows, r#""C:\x|y\hc.exe" attach t1"#, None),
            (Unix, "hc attach t1 extra", None),
            (Unix, "hc attach 't1'", None),
            (Unix, "hc attach t10", None),
            (Unix, "hc  attach t1", None),
            (Unix, "hc read t1", None),
            (Unix, "\"hc attach t1", None),
            (Unix, "", None),
        ] {
            assert_eq!(
                attach_program(command, "t1", os),
                want,
                "{os:?} {command:?}"
            );
        }
        assert_eq!(
            attach_program("hc attach ", "", Unix),
            None,
            "no address attaches nothing"
        );
        assert_eq!(
            attach_program("hc attach t1 extra", "t1 extra", Unix),
            None,
            "an address is one plain token"
        );
    }
    #[test]
    fn identity_rejects_legacy_and_mismatches() {
        let r = record();
        assert!(Identity::read(&r, "bbbbbbbbbbbb", "k:$3").is_none());
        assert!(Identity::read(&r, "aaaaaaaaaaaa", "k:$4").is_none());
        for (k, v) in [
            ("name", json!("Bad Name")),
            ("hcAddress", json!("")),
            ("generation", json!("../0")),
            ("generation", json!("A")),
            ("sessionId", json!("bad")),
            ("launcherArgv", json!([])),
            ("launcherArgv", json!(["", "launcher", "/spec"])),
            ("session", json!("$4")),
        ] {
            let mut bad = r.clone();
            bad[k] = v;
            assert!(
                Identity::read(&bad, "aaaaaaaaaaaa", "k:$3").is_none(),
                "{k}"
            );
        }
        for key in ["team", "generation", "hcAddress", "launcherArgv"] {
            let mut old = r.clone();
            old.as_object_mut().unwrap().remove(key);
            assert!(Identity::read(&old, "aaaaaaaaaaaa", "k:$3").is_none());
        }
        assert!(generation("g-1_ok"));
        assert!(!generation(&"a".repeat(65)));
    }
    #[test]
    fn matches_requires_each_label_and_address() {
        let id = identity();
        let t = Terminal {
            address: id.address.clone(),
            name: None,
            state: crate::hc::State::Live,
            reason: None,
            exit_code: None,
            argv: id.launcher.clone(),
            foreground: None,
            labels: [
                ("mux.managed", "1"),
                ("mux.team", "k:$3"),
                ("mux.id", "aaaaaaaaaaaa"),
                ("mux.gen", "0"),
            ]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .into_iter()
            .collect(),
            pid: None,
        };
        assert!(id.matches(&t));
        for (key, value) in [
            ("mux.managed", "0"),
            ("mux.team", "k:$4"),
            ("mux.id", "bbbbbbbbbbbb"),
            ("mux.gen", "1"),
        ] {
            let mut wrong = t.clone();
            wrong.labels.insert(key.into(), value.into());
            assert!(!id.matches(&wrong), "{key}");
        }
        let mut wrong = t;
        wrong.address = "t2".into();
        assert!(!id.matches(&wrong), "address");
    }
    #[test]
    fn optional_view_requires_exact_nonduplicated_live_command() {
        let r = record();
        let id = identity();
        let row = super::super::registry::Row {
            pane: "%9".into(),
            window: "@5".into(),
            pid: "300".into(),
            dead: false,
            command: "hc attach t1".into(),
            tag: "bbbbbbbbbbbb".into(),
            name: "wrong".into(),
        };
        assert_eq!(
            id.view(&r, std::slice::from_ref(&row), &key()).unwrap().id,
            "aaaaaaaaaaaa"
        );
        assert!(id.view(&r, &[], &key()).is_none());
        assert!(id.view(&r, &[row.clone(), row.clone()], &key()).is_none());
        let mut changed = row.clone();
        changed.command = "human shell".into();
        assert!(id.view(&r, &[changed], &key()).is_none());
        for pid in ["1", "0", ""] {
            let mut invalid = row.clone();
            invalid.pid = pid.into();
            assert!(id.view(&r, &[invalid], &key()).is_none(), "pid {pid:?}");
        }
        let mut invalid_window = row.clone();
        invalid_window.window = "1".into();
        assert!(
            id.view(&r, &[invalid_window], &key()).is_none(),
            "window without @"
        );
        let mut dead = row;
        dead.dead = true;
        assert!(id.view(&r, &[dead], &key()).is_none());
        // Ambiguity 3: a pre-lifecycle record keeps today's rule under any key.
        let anywhere = Key {
            server: "other".into(),
            session: "$9".into(),
        };
        assert!(
            id.view(&r, std::slice::from_ref(&row9()), &anywhere)
                .is_some(),
            "pre-lifecycle: today's rule"
        );
    }

    #[test]
    fn a_generation_carries_its_hc_name() {
        let mut r = record();
        let id = "aaaaaaaaaaaa";
        assert_eq!(
            Identity::read(&r, id, "k:$3").unwrap().hc_name,
            "maaaaaaaa",
            "generation 0: m and id[..8]"
        );
        r["hcName"] = json!("m0123abcd");
        r["generation"] = json!("1");
        let i = Identity::read(&r, id, "k:$3").unwrap();
        assert_eq!(
            (i.hc_name.as_str(), i.generation.as_str()),
            ("m0123abcd", "1")
        );
        for bad in [
            json!("m0123ABCD"),
            json!("m0123abcg"),
            json!("M0123abcd"),
            json!("m0123abc"),
            json!("m0123abcde"),
            json!("x0123abcd"),
            json!("../x"),
            json!(""),
            json!(null),
            json!(7),
        ] {
            r["hcName"] = bad.clone();
            assert!(Identity::read(&r, id, "k:$3").is_none(), "{bad}");
        }
        let mut p = record();
        for k in ["hcAddress", "generation", "launcherArgv", "team"] {
            p.as_object_mut().unwrap().remove(k);
        }
        p["resumedFrom"] = json!("bbbbbbbbbbbb");
        assert_eq!(
            Identity::pending(&p, id, "k:$3").unwrap().hc_name,
            "maaaaaaaa"
        );
    }

    // G2: only an existing generation-0 terminal keeps the derived name; a later generation must carry its own.
    #[test]
    fn a_later_generation_without_its_hc_name_is_refused() {
        let id = "aaaaaaaaaaaa";
        for g in ["1", "2", "10"] {
            let mut r = record();
            r["generation"] = json!(g);
            assert!(
                Identity::read(&r, id, "k:$3").is_none(),
                "generation {g}: no invented name"
            );
            r["hcName"] = json!("m0123abcd");
            assert_eq!(
                Identity::read(&r, id, "k:$3").unwrap().hc_name,
                "m0123abcd",
                "generation {g}"
            );
        }
        let mut r = record();
        r["hcName"] = json!("m0123abcd");
        assert_eq!(
            Identity::read(&r, id, "k:$3").unwrap().hc_name,
            "m0123abcd",
            "a recorded name wins at 0 too"
        );
    }

    // a3c call 47: an identity with no address (a held or pending one, `address: ""`) never matches a terminal by an
    // empty address, even with every label equal.
    #[test]
    fn an_empty_address_never_matches() {
        let mut id = identity();
        id.address = String::new();
        let t = Terminal {
            address: String::new(),
            name: None,
            state: crate::hc::State::Live,
            reason: None,
            exit_code: None,
            argv: id.launcher.clone(),
            foreground: None,
            labels: [
                ("mux.managed", "1"),
                ("mux.team", "k:$3"),
                ("mux.id", "aaaaaaaaaaaa"),
                ("mux.gen", "0"),
            ]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .into_iter()
            .collect(),
            pid: None,
        };
        assert!(!id.matches(&t));
    }
}
