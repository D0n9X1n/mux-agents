//! Readiness barriers for live tests (PM calls 12, 26) and the fake client's own guards (4to7re items 10, 11).
//! `tests/live.rs` (Unix) and `tests/live_windows.rs` each include it; each reaches `support/bounded.rs` as
//! `crate::bounded` (o2 call 14: one bounded runner).
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const BOUND: Duration = Duration::from_secs(60);

/// Member folder `member`'s generation `n` `client.json`, once whole: it parses and names a pid.
pub fn proven(member: &Path, n: u64) -> Option<Value> {
    let text = std::fs::read_to_string(member.join("gens").join(n.to_string()).join("client.json"))
        .ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v["pid"].is_u64().then_some(v)
}

/// PM call 26: waits (bounded) until the launcher published generation `n`'s `client.json`, the evidence member
/// proof reads. Every live helper that commands a member reaches it first, through `wait_member`.
pub fn wait_proof(member: &Path, n: u64) -> Value {
    let end = Instant::now() + BOUND;
    loop {
        if let Some(v) = proven(member, n) {
            return v;
        }
        assert!(
            Instant::now() < end,
            "no client.json for generation {n} of {}",
            member.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The generation a lifecycle record names; None for a pre-lifecycle record, which has no launcher evidence.
pub fn active_gen(record: &Value) -> Option<u64> {
    if record["lifecycle"] != 1 {
        return None;
    }
    record["generation"].as_str()?.parse().ok()
}

/// What one look at member folder `member` shows.
#[derive(Debug, PartialEq)]
enum Look {
    /// No whole record yet, or a lifecycle record whose generation has no whole `client.json` yet.
    Waiting,
    /// A whole pre-lifecycle record: it has no launcher evidence to wait for.
    Pre,
    /// A whole lifecycle record and its generation's `client.json`.
    Proven(Value),
}

fn look(member: &Path) -> Look {
    let text = std::fs::read_to_string(member.join("record.json")).unwrap_or_default();
    let Ok(record @ Value::Object(_)) = serde_json::from_str::<Value>(&text) else {
        return Look::Waiting;
    };
    match active_gen(&record) {
        Some(n) => proven(member, n).map_or(Look::Waiting, Look::Proven),
        None => Look::Pre,
    }
}

/// `wait_proof` for the member in folder `member` at the generation its record names: its `client.json`, or None for
/// a pre-lifecycle member. Checklist 11: an absent or torn record keeps it waiting (bounded), as an unproven one does.
pub fn wait_member(member: &Path) -> Option<Value> {
    let end = Instant::now() + BOUND;
    loop {
        match look(member) {
            Look::Proven(v) => return Some(v),
            Look::Pre => return None,
            Look::Waiting => {}
        }
        assert!(
            Instant::now() < end,
            "no whole record and proof for {}",
            member.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Conversation `sid` holds a whole record line, `<pid> start` or `<pid> resume` ending in a newline (final item 7: the
/// fake creates the file before it writes the line).
pub fn recorded(dir: &Path, sid: &str) -> bool {
    let text =
        std::fs::read_to_string(dir.join(format!("conversation.{sid}.log"))).unwrap_or_default();
    text.split_inclusive('\n').any(|line| {
        let Some(line) = line.strip_suffix('\n') else {
            return false;
        };
        let mut words = line.split(' ');
        let pid = words
            .next()
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        pid && matches!(words.next(), Some("start" | "resume")) && words.next().is_none()
    })
}

/// Waits (bounded) until conversation `sid` is recorded under `dir`. Called before `stop` of a member whose
/// conversation a later `resume` reloads: an unknown resume is refused (PM call 12).
pub fn wait_conversation(dir: &Path, sid: &str) {
    let end = Instant::now() + BOUND;
    while !recorded(dir, sid) {
        assert!(Instant::now() < end, "no conversation {sid} was recorded");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `c` through `bounded::output` (every child in a test is bounded): its exit code, or None when it was killed at
/// `limit` or could not run.
fn exit_code(c: &mut Command, limit: Duration) -> Option<i32> {
    crate::bounded::output(c, limit).ok()?.status.code()
}

/// A fresh folder for one test, under the temp dir.
fn fresh(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mux-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A fresh folder holding `bin/claude` (the fake client, named so it applies claude's rules), and its command.
fn fake_claude(tag: &str) -> (PathBuf, Command) {
    let dir = fresh(&format!("fake-{tag}"));
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let exe = dir.join("bin").join(if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    });
    std::fs::copy(env!("CARGO_BIN_EXE_fake-client"), &exe).unwrap();
    let mut c = Command::new(&exe);
    c.current_dir(&dir)
        .env("MUX_IT_DIR", &dir)
        .env_remove("CLAUDE_CODE_SESSION_ID");
    (dir, c)
}

fn names(dir: &Path) -> Vec<String> {
    let mut n: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    n.sort();
    n
}

// 4to7re item 10, PM call 27: claude's `--resume` of an unknown conversation exits 1 and publishes nothing. Bounded: a
// guard moved after publication leaves the fake running; it is killed at 10 s and the first assertion names it.
#[test]
fn the_fake_refuses_an_unknown_resume_before_publishing() {
    let (dir, mut c) = fake_claude("unknown");
    let code = exit_code(
        c.args(["--resume", "11111111-0000-4000-8000-000000000000"]),
        Duration::from_secs(10),
    );
    assert_eq!(
        names(&dir),
        ["bin"],
        "nothing published: no client record, clients.log or conversation"
    );
    assert_eq!(code, Some(1), "refused at once");
    let _ = std::fs::remove_dir_all(&dir);
}

// 4to7re item 11: a root client with no selector starts CLAUDE_CODE_SESSION_ID's conversation (Live::agent's root).
#[test]
fn the_fake_root_starts_its_session_conversation() {
    let (dir, mut c) = fake_claude("root");
    std::fs::write(dir.join("quit"), "").unwrap();
    let code = exit_code(
        c.env("CLAUDE_CODE_SESSION_ID", "root-sid"),
        Duration::from_secs(30),
    );
    assert_eq!(code, Some(0), "the root client ran and quit");
    assert!(
        recorded(&dir, "root-sid"),
        "no conversation root-sid was recorded"
    );
    let text = std::fs::read_to_string(dir.join("conversation.root-sid.log")).unwrap();
    let words: Vec<&str> = text
        .lines()
        .map(|l| l.split(' ').nth(1).unwrap_or(""))
        .collect();
    assert_eq!(words, ["start"], "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

// final item 7, PM call 26: a conversation counts only with a whole line; a created, unwritten file or a torn line does not.
#[test]
fn a_conversation_is_recorded_only_by_a_whole_line() {
    let dir = fresh("ready-conv");
    let at = dir.join("conversation.s1.log");
    assert!(!recorded(&dir, "s1"), "no file");
    std::fs::write(&at, "").unwrap();
    assert!(!recorded(&dir, "s1"), "an empty file");
    std::fs::write(&at, "41 sta").unwrap();
    assert!(!recorded(&dir, "s1"), "a torn line");
    std::fs::write(&at, "41 start\n").unwrap();
    assert!(recorded(&dir, "s1"), "a whole line");
    let _ = std::fs::remove_dir_all(&dir);
}

// PM call 26: a member is commandable only with a whole client.json of its generation; a pre-lifecycle record waits
// for none.
#[test]
fn a_member_is_proven_only_by_a_whole_client_record() {
    let member = fresh("ready-proof");
    let gen0 = member.join("gens").join("0");
    std::fs::create_dir_all(&gen0).unwrap();
    assert!(proven(&member, 0).is_none(), "no file");
    std::fs::write(gen0.join("client.json"), "").unwrap();
    assert!(proven(&member, 0).is_none(), "an empty file");
    std::fs::write(gen0.join("client.json"), r#"{"pid":"#).unwrap();
    assert!(proven(&member, 0).is_none(), "a torn record");
    std::fs::write(gen0.join("client.json"), r#"{"pid":41}"#).unwrap();
    assert_eq!(
        proven(&member, 0).map(|v| v["pid"].clone()),
        Some(serde_json::json!(41)),
        "a whole record"
    );
    assert_eq!(
        active_gen(&serde_json::json!({"lifecycle": 1, "generation": "2"})),
        Some(2),
        "a lifecycle record"
    );
    assert_eq!(
        active_gen(&serde_json::json!({"generation": "2"})),
        None,
        "a pre-lifecycle record waits for none"
    );
    let _ = std::fs::remove_dir_all(&member);
}

// Checklist 11: a member folder with no record, a torn record, or a lifecycle record without its client.json keeps
// `wait_member` waiting; it returns the proof once both are whole, and returns at once for a pre-lifecycle record.
#[test]
fn a_member_waits_for_a_whole_record_and_its_proof() {
    let member = fresh("ready-wait");
    assert_eq!(look(&member), Look::Waiting, "no record");
    std::fs::write(member.join("record.json"), r#"{"lifecycle":1,"gen"#).unwrap();
    assert_eq!(look(&member), Look::Waiting, "a torn record");
    std::fs::write(member.join("record.json"), "").unwrap();
    assert_eq!(look(&member), Look::Waiting, "an empty record");
    std::fs::remove_file(member.join("record.json")).unwrap();
    let writer = {
        let member = member.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            std::fs::write(
                member.join("record.json"),
                r#"{"lifecycle":1,"generation":"0"}"#,
            )
            .unwrap();
            std::thread::sleep(Duration::from_millis(300));
            let gen0 = member.join("gens").join("0");
            std::fs::create_dir_all(&gen0).unwrap();
            std::fs::write(gen0.join("client.json"), r#"{"pid":41}"#).unwrap();
        })
    };
    let got = wait_member(&member);
    writer.join().unwrap();
    assert_eq!(
        got.map(|v| v["pid"].clone()),
        Some(serde_json::json!(41)),
        "waited for the record, then its proof"
    );
    std::fs::write(member.join("record.json"), r#"{"generation":"2"}"#).unwrap();
    assert_eq!(look(&member), Look::Pre, "a pre-lifecycle record");
    assert_eq!(
        wait_member(&member),
        None,
        "a pre-lifecycle record waits for none"
    );
    let _ = std::fs::remove_dir_all(&member);
}
