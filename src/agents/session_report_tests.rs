//! report-2b K3, K3a, K4 through the real CLI (`cli::run`): fake proc, clock and hc; real files.
use super::core_fixture as cf;
use super::fixture::World;
use super::gens::{self, path};
use super::session_report::{Input, Labels, hooks, labels, read_capped, read_input};
use crate::parse::Client;
use crate::state::evidence::publish;
use crate::state::live;
use crate::testing::{Wrong, plant, untouched};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

const KEY: &str = "0123456789abcdef";
const ID: &str = "aaaaaaaaaaaa";
const NEW: &str = "0f11fc2f-2032-48ef-a920-4af2a8e5af50";
const OLD: &str = "1e22fc2f-2032-48ef-a920-4af2a8e5af51";
const ADDR: &str = "taaaaaaaaaaaa";

/// `mux-agent <args>` as the binary runs it: exit code and the parsed reply (`Null` for `-Quiet`'s empty output).
fn call(w: &World, args: &[&str]) -> (u8, Value) {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let (code, out) = crate::cli::run(&args, || Ok(&w.ctx));
    (code, serde_json::from_str(&out).unwrap_or(Value::Null))
}

/// An hc terminal, no multiplexer: caller 4242 > shell 170 > claude 150 (start 1500) > 100 (start 1000, the launcher).
fn hc_world() -> World {
    let mut w = World::agent("");
    w.ctx.env = crate::ctx::Env::from_pairs([("HC_TERMINAL_ID", ADDR)]);
    w
}

/// The same without a client: caller > shell > 100 (a shell) > server 900 (neither).
fn hc_human() -> World {
    let mut w = World::human("");
    w.ctx.env = crate::ctx::Env::from_pairs([("HC_TERMINAL_ID", ADDR)]);
    w
}

/// One hc connect (`--version`, then `get`) answering terminal ADDR with `labels`, in `cf::wire`'s shape.
fn terminal(w: &World, labels: Value) {
    cf::online(w);
    cf::get(
        w,
        ADDR,
        json!({"address": ADDR, "state": "live", "argv": [], "labels": labels}),
    );
}

fn gen_labels(n: &str) -> Value {
    json!({"mux.managed": "1", "mux.team": format!("{KEY}:$3"), "mux.id": ID, "mux.gen": n, "mux.name": "w1"})
}

fn member(w: &World) -> PathBuf {
    w.ctx.state_root.join(KEY).join("agents").join(ID)
}

fn gen_file(w: &World, file: &str) -> PathBuf {
    path(&member(w), 1, file)
}

fn write_record(w: &World, record: Value) {
    // PM call 16: the state root and the member folder are made 0700 on Unix, never by create_dir_all alone.
    crate::testing::private_tree(&w.ctx.state_root, &member(w));
    crate::state::fs::write_json(&member(w).join("record.json"), &record).unwrap();
}

/// Generation 1's `spec.json` (session `sid`, deadline now + 150 s, as `forward` writes it) and `launcher.json`.
fn gen1(w: &World, sid: &str) {
    let deadline = crate::util::utc_o(w.ctx.clock.now() + Duration::from_secs(150));
    publish(
        &gen_file(w, "spec"),
        &json!({"sessionId": sid, "kind": "spawn", "deadline": deadline}),
    )
    .unwrap();
    publish(
        &gen_file(w, "launcher"),
        &json!({"pid": 100, "start": 1000, "startedAt": "x"}),
    )
    .unwrap();
}

/// A new member's pending generation 1 (the shape A3a's gens tests accept), launched, no `client.json` yet.
fn pending(w: &World) {
    write_record(
        w,
        json!({"lifecycle": 1, "id": ID, "pending": {"gen": 1, "sessionId": NEW}}),
    );
    gen1(w, NEW);
}

fn client_json(w: &World) {
    publish(&gen_file(w, "client"), &json!({"pid": 150, "start": 1500})).unwrap();
}

fn claude_entry(w: &World) -> PathBuf {
    live::path(&w.ctx, Client::Claude, 150, 1500)
}

// Real CLI: an unmanaged hc terminal (no mux.* labels) writes only its registry entry; seq counts; no member verb.
#[test]
fn an_unmanaged_hc_terminal_writes_only_its_registry_entry() {
    let w = hc_world();
    terminal(&w, json!({}));
    let (code, reply) = call(&w, &["report", "-Session", NEW, "-Source", "startup"]);
    assert_eq!(code, 0, "{reply}");
    assert_eq!(
        reply,
        json!({"ok": true, "verb": "report", "sessionId": NEW, "source": "startup",
            "client": {"kind": "claude", "pid": 150, "start": 1500}, "seq": 1, "handshake": "none", "ready": null})
    );
    terminal(&w, json!({}));
    let (_, reply) = call(&w, &["report", "-Session", NEW, "-Source", "resume"]);
    assert_eq!(reply["seq"], 2);
    let e = crate::state::fs::read_json(&claude_entry(&w))
        .unwrap()
        .unwrap();
    assert_eq!(
        (&e["seq"], &e["source"], &e["sessionId"]),
        (&json!(2), &json!("resume"), &json!(NEW))
    );
    assert!(!w.ctx.state_root.join(KEY).exists(), "no member file");
    terminal(&w, json!({}));
    let (code, reply) = call(&w, &["report", "idle"]);
    assert_eq!(
        (code, &reply["check"]),
        (2, &json!("herdr")),
        "no member verb"
    );
}

// K3 and PM call 9: no client, an unreadable row on the walk, or an unresolved identity: exit 0, nothing written,
// no hc call (labels are read only after a client is found).
#[test]
fn no_client_or_an_unreadable_walk_writes_nothing_and_exits_0() {
    let w = hc_human();
    terminal(&w, gen_labels("1")); // queued: an hc call before the walk would show below
    pending(&w);

    client_json(&w);
    let (code, reply) = call(&w, &["report", "-Session", NEW, "-Source", "startup"]);
    assert_eq!(
        (code, &reply["client"], &reply["seq"]),
        (0, &Value::Null, &Value::Null),
        "{reply}"
    );
    assert_eq!(
        call(&w, &["report", "-Session", NEW, "-Quiet"]),
        (0, Value::Null),
        "the hook form"
    );
    let w2 = hc_world();
    terminal(&w2, gen_labels("1"));
    w2.fakes.forget(170);
    w2.fakes.process(crate::testing::unknown_start(170, 150));
    let (code, reply) = call(&w2, &["report", "-Session", NEW]);
    assert_eq!(
        (code, &reply["client"]),
        (0, &json!("unreadable")),
        "walk: {reply}"
    );
    let w3 = hc_world();
    terminal(&w3, gen_labels("1"));
    w3.fakes.opens(150, &[Some(1499)]);
    let (code, reply) = call(&w3, &["report", "-Session", NEW]);
    assert_eq!(
        (code, &reply["client"]),
        (0, &json!("unreadable")),
        "identity: {reply}"
    );
    for w in [&w, &w2, &w3] {
        assert!(!live::dir(&w.ctx).exists(), "no registry entry");
        assert!(!gen_file(w, "report").exists() && !gen_file(w, "ready").exists());
        assert!(w.fakes.hc_calls().is_empty());
    }
}

// The Copilot hook's form: `{sessionId, source}` on stdin. Bad input is usage and writes nothing.
#[test]
fn the_copilot_stdin_form_reads_session_and_source() {
    let w = hc_world();
    w.fakes.client(150, Client::Copilot);
    hooks::stdin(Some(
        format!(r#"{{"sessionId":"{NEW}","timestamp":1,"cwd":"/w","source":"resume"}}"#).as_bytes(),
    ));
    terminal(&w, json!({}));
    let (code, reply) = call(&w, &["report", "-Session", "-Stdin"]);
    assert_eq!(
        (code, &reply["source"], &reply["client"]["kind"]),
        (0, &json!("resume"), &json!("copilot"))
    );
    let file = live::path(&w.ctx, Client::Copilot, 150, 1500);
    assert_eq!(
        crate::state::fs::read_json(&file).unwrap().unwrap()["sessionId"],
        NEW
    );
    let bad: [Option<&[u8]>; 5] = [
        Some(&br#"{"sessionId":"x"}"#[..]),
        Some(&b"{}"[..]),
        Some(&b"not json"[..]),
        Some(&b""[..]),
        None,
    ];
    for input in bad {
        hooks::stdin(input);
        let (code, reply) = call(&w, &["report", "-Session", "-Stdin"]);
        assert_eq!((code, &reply["check"]), (2, &json!("usage")), "{input:?}");
    }
    assert_eq!(
        crate::state::fs::read_json(&file).unwrap().unwrap()["seq"],
        1,
        "nothing more written"
    );
    hooks::stdin(Some(
        format!(
            r#"{{"sessionId":"{}","source":"Re sume"}}"#,
            NEW.to_uppercase()
        )
        .as_bytes(),
    ));
    terminal(&w, json!({}));
    let (_, reply) = call(&w, &["report", "-Session", "-Stdin"]);
    assert_eq!(
        (&reply["sessionId"], &reply["source"]),
        (&json!(NEW), &json!("unknown")),
        "lowercased; bad source"
    );
}

// K3a: dispatched after the sandbox check and before caller proof (only the labels' connect reaches hc).
#[test]
fn the_session_form_skips_caller_proof_and_keeps_the_sandbox_check() {
    let w = hc_world();
    terminal(&w, json!({}));
    let (code, reply) = call(&w, &["report", "-Session", NEW]);
    assert_eq!(code, 0, "{reply}");
    assert_eq!(
        w.fakes.hc_calls().len(),
        2,
        "--version and get, for the labels only"
    );
    let mut w = hc_world();
    w.ctx.env = crate::ctx::Env::from_pairs([("HC_TERMINAL_ID", ADDR), ("SANDBOX_RUNTIME", "1")]);
    let (code, reply) = call(&w, &["report", "-Session", NEW]);
    assert_eq!((code, &reply["check"]), (2, &json!("sandbox")));
    assert!(!live::dir(&w.ctx).exists() && w.fakes.hc_calls().is_empty());
}

// (fix 1) Ambiguity 5, task-local: a registry entry that cannot be written does not stop the verb.
#[test]
fn a_registry_error_still_reads_the_labels() {
    let w = hc_world();
    assert_eq!(
        live::report(&w.ctx, Client::Claude, 150, 1500, NEW, "startup").map_err(|r| r.check),
        Ok(1)
    );
    std::fs::write(claude_entry(&w), b"{").unwrap();
    terminal(&w, json!({}));
    let (code, reply) = call(&w, &["report", "-Session", NEW]);
    assert_eq!(
        (code, &reply["seq"], &reply["handshake"]),
        (0, &json!("evidence"), &json!("none")),
        "{reply}"
    );
    assert_eq!(w.fakes.hc_calls().len(), 2, "the labels were still read");
    assert_eq!(
        std::fs::read(claude_entry(&w)).unwrap(),
        b"{",
        "kept as it was"
    );
}

// (fix 1) Ambiguity 4, task-local: labels only in an hc terminal; none is unmanaged; partial, malformed or an hc
// error is Unreadable.
#[test]
fn labels_are_read_only_in_an_hc_terminal_and_a_partial_set_is_unreadable() {
    let w = World::agent("");
    assert_eq!(labels(&w.ctx), Labels::None, "tmux: not an hc terminal");
    assert!(w.fakes.hc_calls().is_empty());
    let mut w = World::agent("");
    w.ctx.env = crate::ctx::Env::from_pairs([("HC_TERMINAL_ID", ADDR), ("TMUX", "/tmp/x,1,0")]);
    terminal(&w, gen_labels("1"));
    assert_eq!(
        labels(&w.ctx),
        Labels::None,
        "an hc id inside tmux is not an hc terminal"
    );
    assert!(
        w.fakes.hc_calls().is_empty(),
        "no hc call outside an hc terminal"
    );
    let w = hc_world();
    terminal(&w, gen_labels("1"));
    assert_eq!(
        labels(&w.ctx),
        Labels::Gen {
            key: KEY.into(),
            id: ID.into(),
            n: 1
        },
        "a full set"
    );
    terminal(&w, json!({}));
    assert_eq!(labels(&w.ctx), Labels::None, "no mux.* label: unmanaged");
    let bad = [
        json!({"mux.team": format!("{KEY}:$3"), "mux.id": ID}),
        json!({"mux.gen": "1"}),
        json!({"mux.team": format!("{KEY}:$3"), "mux.id": ID, "mux.gen": "../1"}),
        json!({"mux.team": "short:$3", "mux.id": ID, "mux.gen": "1"}),
        json!({"mux.team": format!("{KEY}:$3"), "mux.id": "AAAA", "mux.gen": "1"}),
    ];
    for l in bad {
        terminal(&w, l.clone());
        assert_eq!(labels(&w.ctx), Labels::Unreadable, "{l}");
    }
    let w = hc_world(); // no hc installed: connect refuses
    assert_eq!(labels(&w.ctx), Labels::Unreadable, "hc unreachable");
}

// (fix 1) Task-local: a tmux or rmux pane is registry only; nothing reaches hc or the multiplexer.
#[test]
fn a_tmux_pane_writes_only_its_registry_entry() {
    let w = World::agent("");
    let (code, reply) = call(&w, &["report", "-Session", NEW]);
    assert_eq!(
        (code, &reply["seq"], &reply["handshake"]),
        (0, &json!(1), &json!("none")),
        "{reply}"
    );
    assert!(w.fakes.hc_calls().is_empty() && w.fakes.mux_calls().is_empty());
    assert!(claude_entry(&w).is_file());
}

// (fix 1) Ambiguity 17, task-local, on the production reader: never a terminal, at most 64 KiB.
#[test]
fn stdin_is_refused_from_a_terminal_and_capped_at_64_kib() {
    let at = |n: usize| read_capped(false, std::io::Cursor::new(vec![b' '; n])).map(|b| b.len());
    assert_eq!(at(65_536), Some(65_536), "exactly 64 KiB");
    assert_eq!(at(65_537), None, "one byte more");
    assert_eq!(at(0), Some(0));
    assert_eq!(
        read_capped(true, std::io::Cursor::new(b"{}".to_vec())),
        None,
        "a terminal"
    );
}

// (fix 2, re-audit 5) Through dispatch: partial or malformed labels, or an hc error, answer `handshake: "evidence"`
// (the registry entry is still written), never "none".
#[test]
fn unreadable_labels_answer_evidence_through_the_cli() {
    let bad = [
        json!({"mux.team": format!("{KEY}:$3"), "mux.id": ID}),
        json!({"mux.team": format!("{KEY}:$3"), "mux.id": ID, "mux.gen": "../1"}),
    ];
    for l in bad {
        let w = hc_world();
        terminal(&w, l.clone());
        let (code, reply) = call(&w, &["report", "-Session", NEW]);
        let got = (code, &reply["seq"], &reply["handshake"], &reply["ready"]);
        assert_eq!(got, (0, &json!(1), &json!("evidence"), &Value::Null), "{l}");
    }
    let w = hc_world(); // no hc installed: connect refuses
    let (code, reply) = call(&w, &["report", "-Session", NEW]);
    assert_eq!(
        (code, &reply["seq"], &reply["handshake"]),
        (0, &json!(1), &json!("evidence")),
        "hc error: {reply}"
    );
}

// (fix 2, re-audit 6) Bounded consumption: from an endless input the reader takes 64 KiB + 1 bytes and stops.
#[test]
fn the_reader_consumes_at_most_64_kib_plus_one() {
    struct Endless(std::rc::Rc<std::cell::Cell<usize>>);
    impl std::io::Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(4096);
            buf[..n].fill(b' ');
            self.0.set(self.0.get() + n);
            assert!(self.0.get() <= 1 << 20, "read past 1 MiB: unbounded");
            Ok(n)
        }
    }
    let count = std::rc::Rc::new(std::cell::Cell::new(0));
    assert_eq!(
        read_capped(false, Endless(count.clone())),
        None,
        "more than 64 KiB is no input"
    );
    assert_eq!(count.get(), 65_537, "consumed exactly 64 KiB + 1");
}

// (fix 2, re-audit 7) The production wrapper asks its input whether it is a terminal (`read_input`, which
// `read_stdin` calls with the real stdin): a file is read ...
#[test]
fn read_input_reads_a_file_it_is_given() {
    let d = crate::testing::TempDir::new();
    let p = d.path().join("in.json");
    std::fs::write(&p, b"{}").unwrap();
    assert_eq!(
        read_input(std::fs::File::open(&p).unwrap()),
        Some(b"{}".to_vec())
    );
}

// ... and a real terminal (a pty's slave) is refused at once, never read. Windows: CI gives a child no console; the
// manual check is in `-6.md` Step 4.
#[cfg(unix)]
#[test]
fn read_input_refuses_a_real_terminal_without_reading() {
    use std::os::fd::FromRawFd;
    let (mut master, mut slave) = (0, 0);
    // SAFETY: two valid out pointers; the name, termios and window size are not asked for.
    let made = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(made, 0, "openpty");
    // SAFETY: openpty returned these descriptors and nothing else owns them.
    let (master, slave) = unsafe {
        (
            std::fs::File::from_raw_fd(master),
            std::fs::File::from_raw_fd(slave),
        )
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_input(slave));
    });
    let got = rx.recv_timeout(std::time::Duration::from_secs(2));
    drop(master); // a read blocked on the slave ends, and its thread exits
    assert_eq!(got, Ok(None), "a terminal is refused at once, never read");
}

/// A reader that says whether it is a terminal, so the refusal is tested on every OS (build checklist item 11).
struct Fake {
    terminal: bool,
    bytes: std::io::Cursor<Vec<u8>>,
}

impl std::io::Read for Fake {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.bytes.read(buf)
    }
}

impl Input for Fake {
    fn is_terminal(&self) -> bool {
        self.terminal
    }
}

// Build checklist item 11: the production wrapper asks the input it reads. A terminal is refused even with a valid
// body; the same body from a pipe is read.
#[test]
fn read_input_refuses_a_terminal_reader_on_every_os() {
    let body = format!(r#"{{"sessionId":"{NEW}"}}"#).into_bytes();
    let fake = |terminal| Fake {
        terminal,
        bytes: std::io::Cursor::new(body.clone()),
    };
    assert_eq!(read_input(fake(false)), Some(body.clone()), "a pipe");
    assert_eq!(read_input(fake(true)), None, "a terminal");
}

/// One `report -Session NEW` from generation `n`'s terminal (one hc connect for the labels).
fn report_gen(w: &World, n: &str) -> (u8, Value) {
    terminal(w, gen_labels(n));
    call(w, &["report", "-Session", NEW, "-Source", "startup"])
}

fn exists(w: &World, file: &str) -> bool {
    gen_file(w, file).exists()
}

fn json_of(w: &World, file: &str) -> Value {
    crate::state::fs::read_json(&gen_file(w, file))
        .unwrap()
        .unwrap()
}

// Real CLI from a pending generation's terminal with no client.json: report.json, no `tagged`; no member verb.
#[test]
fn a_pending_terminal_reports_without_member_proof() {
    let w = hc_world();
    pending(&w);
    let (code, reply) = report_gen(&w, "1");
    assert_eq!(
        (code, &reply["handshake"], &reply["ready"], &reply["seq"]),
        (0, &json!("reported"), &json!("undecided"), &json!(1)),
        "{reply}"
    );
    let r = json_of(&w, "report");
    assert_eq!(
        (&r["sessionId"], &r["pid"], &r["start"]),
        (&json!(NEW), &json!(150), &json!(1500))
    );
    assert!(!exists(&w, "ready"));
    assert_eq!(w.fakes.slept(), vec![200; 50], "the whole 10 s poll");
    for verb in [&["report", "idle"][..], &["read"][..]] {
        terminal(&w, gen_labels("1"));
        let (code, reply) = call(&w, verb);
        assert_eq!(
            (code, &reply["check"]),
            (2, &json!("herdr")),
            "{verb:?}: no member verb"
        );
    }
}

// launch-1 G1.5: the record keeps OLD until promotion; generation 1's spec.json says NEW, and that decides.
#[test]
fn the_spec_session_decides_not_the_record() {
    let w = hc_world();
    write_record(
        &w,
        json!({"lifecycle": 1, "id": ID, "generation": "0", "hcName": "m0123abcd",
        "sessionId": OLD, "pending": {"gen": 1, "sessionId": NEW}}),
    );
    gen1(&w, NEW);
    client_json(&w);
    terminal(&w, gen_labels("1"));
    let (_, reply) = call(&w, &["report", "-Session", OLD]);
    assert_eq!(
        (&reply["handshake"], &reply["ready"]),
        (&json!("session"), &Value::Null),
        "the record's session"
    );
    assert!(!exists(&w, "report") && !exists(&w, "ready"));
    let (_, reply) = report_gen(&w, "1");
    assert_eq!(
        (&reply["handshake"], &reply["ready"]),
        (&json!("reported"), &json!("ok")),
        "{reply}"
    );
    // 0 is the active generation but has no spec.json; 2 is neither: both stale (ambiguity 12).
    for n in ["0", "2"] {
        assert_eq!(report_gen(&w, n).1["handshake"], "stale", "label {n}");
    }
}

#[test]
fn an_early_report_is_promoted_when_client_json_lands_within_10_s() {
    let w = hc_world();
    pending(&w);
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let l = log.clone();
    w.fakes.lock_probe(move |e| {
        l.borrow_mut().push(e.to_string());
        String::new()
    });
    let (l, m) = (log.clone(), member(&w));
    w.fakes.sleep_probe(move |count| {
        l.borrow_mut().push(format!("sleep {count}"));
        if count == 3 {
            publish(&path(&m, 1, "client"), &json!({"pid": 150, "start": 1500})).unwrap();
        }
    });
    let (code, reply) = report_gen(&w, "1");
    assert_eq!((code, &reply["ready"]), (0, &json!("ok")), "{reply}");
    let g = crate::agents::team_state::gen_lock_key(KEY, ID, 1);
    let (plus, minus) = (format!("+{g}"), format!("-{g}"));
    let want = [
        "+live|150-1500",
        "-live|150-1500",
        plus.as_str(),
        minus.as_str(),
        "sleep 1",
        "sleep 2",
        "sleep 3",
        plus.as_str(),
        minus.as_str(),
    ];
    assert_eq!(
        *log.borrow(),
        want,
        "no lock is held while polling; the poll stops at the client"
    );
    let r = json_of(&w, "ready");
    assert_eq!((&r["pid"], &r["start"]), (&json!(150), &json!(1500)));
}

#[test]
fn a_client_json_after_the_wait_is_promoted_by_a_later_call() {
    let w = hc_world();
    pending(&w);
    assert_eq!(report_gen(&w, "1").1["ready"], "undecided");
    w.fakes.advance(Duration::from_secs(20)); // after the 10 s wait, before the 150 s deadline
    client_json(&w);
    assert_eq!(
        gens::promote_ready(&w.ctx, KEY, ID, 1).unwrap(),
        gens::Ready::Ok
    );
    assert!(exists(&w, "ready"));
}

#[test]
fn a_client_json_that_never_lands_leaves_the_verdict_undecided() {
    let w = hc_world();
    pending(&w);
    report_gen(&w, "1");
    let first = std::fs::read(gen_file(&w, "report")).unwrap();
    w.fakes.advance(Duration::from_secs(300));
    assert_eq!(
        gens::promote_ready(&w.ctx, KEY, ID, 1).unwrap(),
        gens::Ready::Undecided,
        "outcome-2's"
    );
    assert!(!exists(&w, "ready"));
    assert_eq!(
        report_gen(&w, "1").1["handshake"],
        "closed",
        "past the deadline"
    );
    assert_eq!(
        std::fs::read(gen_file(&w, "report")).unwrap(),
        first,
        "report.json written once"
    );
}

// failure.json is published under the gen lock while the verb polls; client.json lands after it.
#[test]
fn a_failure_published_during_the_wait_wins() {
    let w = hc_world();
    pending(&w);
    let (m, g) = (
        member(&w),
        crate::agents::team_state::gen_lock_key(KEY, ID, 1),
    );
    w.fakes.sleep_probe(move |count| {
        if count == 2 {
            crate::testing::locked(&g, || {
                publish(
                    &path(&m, 1, "failure"),
                    &json!({"reason": "no readiness report"}),
                )
                .map(|_| ())
            })
            .unwrap();
        }
        if count == 3 {
            publish(&path(&m, 1, "client"), &json!({"pid": 150, "start": 1500})).unwrap();
        }
    });
    let (code, reply) = report_gen(&w, "1");
    assert_eq!((code, &reply["ready"]), (0, &json!("failed")), "{reply}");
    assert!(exists(&w, "report") && !exists(&w, "ready"));
}

// K4.1 and ambiguities 10-12: nothing past the registry is written.
#[test]
fn a_closed_stale_or_unreadable_generation_gets_no_report() {
    type Setup = fn(&World);
    let cases: [(&str, &str, Setup); 8] = [
        ("outcome.json", "closed", |w| {
            publish(&gen_file(w, "outcome"), &json!({"verdict": "ok"}))
                .map(drop)
                .unwrap()
        }),
        ("failure.json", "closed", |w| {
            publish(&gen_file(w, "failure"), &json!({"reason": "x"}))
                .map(drop)
                .unwrap()
        }),
        ("now = deadline", "closed", |w| {
            w.fakes.advance(Duration::from_secs(150))
        }),
        ("torn outcome.json", "evidence", |w| {
            std::fs::write(gen_file(w, "outcome"), b"{").unwrap()
        }),
        ("spec without a deadline", "evidence", |w| {
            std::fs::remove_file(gen_file(w, "spec")).unwrap();
            publish(&gen_file(w, "spec"), &json!({"sessionId": NEW})).unwrap();
        }),
        ("torn record", "evidence", |w| {
            std::fs::write(member(w).join("record.json"), b"{").unwrap()
        }),
        (
            "spec of a generation neither pending nor active",
            "stale",
            |w| {
                write_record(
                    w,
                    json!({"lifecycle": 1, "id": ID, "pending": {"gen": 2, "sessionId": NEW}}),
                )
            },
        ),
        ("no record", "stale", |w| {
            std::fs::remove_file(member(w).join("record.json")).unwrap()
        }),
    ];
    for (case, want, setup) in cases {
        let w = hc_world();
        pending(&w);
        client_json(&w);
        setup(&w);
        let (code, reply) = report_gen(&w, "1");
        assert_eq!(
            (code, &reply["handshake"], &reply["seq"]),
            (0, &json!(want), &json!(1)),
            "{case}: {reply}"
        );
        assert!(!exists(&w, "report") && !exists(&w, "ready"), "{case}");
    }
    let w = hc_world();
    pending(&w);
    terminal(&w, json!({"mux.team": format!("{KEY}:$3"), "mux.id": ID}));
    let (_, reply) = call(&w, &["report", "-Session", NEW]);
    assert_eq!(reply["handshake"], "evidence", "partial labels");
    assert!(!exists(&w, "report"));
}

// Ambiguity 5: a torn registry entry is kept as it is, and readiness goes on.
#[test]
fn a_torn_registry_entry_does_not_stop_the_handshake() {
    let w = hc_world();
    pending(&w);
    client_json(&w);
    live::report(&w.ctx, Client::Claude, 150, 1500, NEW, "startup").unwrap();
    std::fs::write(claude_entry(&w), b"{").unwrap();
    let (code, reply) = report_gen(&w, "1");
    assert_eq!(
        (code, &reply["seq"], &reply["handshake"], &reply["ready"]),
        (0, &json!("evidence"), &json!("reported"), &json!("ok")),
        "{reply}"
    );
    assert_eq!(std::fs::read(claude_entry(&w)).unwrap(), b"{");
}

// (fix 1, audit 5-9 item 3) S9: report.json is published while the gen lock is held: it exists at the lock's release.
#[test]
fn report_json_is_published_inside_the_gen_lock() {
    let w = hc_world();
    pending(&w);
    client_json(&w);
    let (seen, at) = (
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
        gen_file(&w, "report"),
    );
    let (s, release) = (
        seen.clone(),
        format!("-{}", crate::agents::team_state::gen_lock_key(KEY, ID, 1)),
    );
    w.fakes.lock_probe(move |e| {
        if e == release {
            s.borrow_mut().push(at.exists());
        }
        String::new()
    });
    assert_eq!(report_gen(&w, "1").1["handshake"], "reported");
    assert_eq!(
        seen.borrow().first(),
        Some(&true),
        "report.json exists at the first gen-lock release"
    );
}

// (fix 1, audit 5-9 item 4) R4: the active generation's terminal is accepted too, not only a pending one.
#[test]
fn the_active_generation_is_accepted_too() {
    let w = hc_world();
    write_record(
        &w,
        json!({"lifecycle": 1, "id": ID, "generation": "1", "hcName": "m0123abcd", "sessionId": NEW}),
    );
    gen1(&w, NEW);
    client_json(&w);
    let (_, reply) = report_gen(&w, "1");
    assert_eq!(
        (&reply["handshake"], &reply["ready"]),
        (&json!("reported"), &json!("ok")),
        "{reply}"
    );
}

// (fix 1, PM call 14) A failed flush of gens/1 is a failed publication: no `reported`, no ready.json; the next call
// flushes and goes on.
#[test]
fn a_report_json_that_cannot_be_flushed_is_not_reported() {
    let w = hc_world();
    pending(&w);
    client_json(&w);
    crate::state::fs::fail_sync_dir(Some(&gens::dir(&member(&w), 1)));
    let (code, reply) = report_gen(&w, "1");
    crate::state::fs::fail_sync_dir(None);
    assert_eq!(code, 0, "{reply}");
    assert_ne!(
        reply["handshake"], "reported",
        "a failed flush is a failed publication"
    );
    assert!(!exists(&w, "ready"));
    let (_, reply) = report_gen(&w, "1");
    assert_eq!(
        (&reply["handshake"], &reply["ready"]),
        (&json!("reported"), &json!("ok")),
        "retried and flushed"
    );
}

// (fix 2, re-audit 5) K4.1, R2 and PM call 13: a torn or wrong-type outcome.json, failure.json or spec.json refuses
// `evidence` before report.json is published, and nothing is written through a planted link.
#[test]
fn unreadable_k4_evidence_refuses_before_report_json() {
    for file in ["outcome", "failure", "spec"] {
        let mut kinds: Vec<Option<Wrong>> = vec![None];
        kinds.extend(Wrong::all().into_iter().map(Some));
        for kind in kinds {
            let w = hc_world();
            pending(&w);
            client_json(&w);
            let at = gen_file(&w, file);
            let _ = std::fs::remove_file(&at);
            match kind {
                None => std::fs::write(&at, b"{").unwrap(),
                Some(k) => plant(k, &at),
            }
            let (code, reply) = report_gen(&w, "1");
            assert_eq!(
                (code, &reply["handshake"], &reply["ready"]),
                (0, &json!("evidence"), &Value::Null),
                "{file} {kind:?}: {reply}"
            );
            assert!(
                !exists(&w, "report") && !exists(&w, "ready"),
                "{file} {kind:?}: nothing published"
            );
            assert!(untouched(&at), "{file} {kind:?}: nothing written through");
        }
    }
}

// (fix 2, re-audit 6) R1: the handshake reads the member's record.json once and generation 1's spec.json once.
#[test]
fn the_handshake_reads_the_member_and_its_spec_once() {
    let w = hc_world();
    pending(&w);
    client_json(&w);
    let (record, spec) = (member(&w).join("record.json"), gen_file(&w, "spec"));
    let ((code, reply), paths) =
        crate::state::evidence::record_evidence_reads(|| report_gen(&w, "1"));
    assert_eq!(
        (code, &reply["handshake"]),
        (0, &json!("reported")),
        "{reply}"
    );
    let count = |p: &PathBuf| paths.iter().filter(|q| *q == p).count();
    assert_eq!((count(&record), count(&spec)), (1, 1), "{paths:?}");
}

// Build checklist item 2: generation 1's spec.json may carry its session in upper case; it is lowercased and still
// matches the verb's (lowercased) session.
#[test]
fn an_uppercase_spec_session_still_matches() {
    let w = hc_world();
    write_record(
        &w,
        json!({"lifecycle": 1, "id": ID, "pending": {"gen": 1, "sessionId": NEW}}),
    );
    gen1(&w, &NEW.to_uppercase());
    client_json(&w);
    let (code, reply) = report_gen(&w, "1");
    assert_eq!(
        (code, &reply["handshake"], &reply["ready"]),
        (0, &json!("reported"), &json!("ok")),
        "{reply}"
    );
}

// Build checklist item 3: a deadline that is present but cannot be parsed refuses `evidence`; nothing is published.
#[test]
fn an_unparsable_deadline_refuses_evidence_and_publishes_nothing() {
    let w = hc_world();
    pending(&w);
    client_json(&w);
    std::fs::remove_file(gen_file(&w, "spec")).unwrap();
    publish(
        &gen_file(&w, "spec"),
        &json!({"sessionId": NEW, "deadline": "tomorrow"}),
    )
    .unwrap();
    let (code, reply) = report_gen(&w, "1");
    assert_eq!(
        (code, &reply["handshake"], &reply["ready"]),
        (0, &json!("evidence"), &Value::Null),
        "{reply}"
    );
    assert!(!exists(&w, "report") && !exists(&w, "ready"));
}

// Build checklist item 4: a gen lock that cannot be taken answers `handshake: "lock"`; nothing is published.
#[test]
fn a_gen_lock_timeout_answers_lock_and_publishes_nothing() {
    let w = hc_world();
    pending(&w);
    client_json(&w);
    w.fakes.lock_timeout("gen|");
    let (code, reply) = report_gen(&w, "1");
    assert_eq!(
        (code, &reply["handshake"], &reply["ready"]),
        (0, &json!("lock"), &Value::Null),
        "{reply}"
    );
    assert!(!exists(&w, "report") && !exists(&w, "ready"));
    assert!(
        claude_entry(&w).is_file(),
        "the registry entry is still written"
    );
}

#[test]
fn killed_after_the_registry_write_leaves_no_report_and_nothing_is_replayed() {
    let w = hc_world();
    pending(&w);
    hooks::stop_after(Some("registry"));
    let (code, reply) = report_gen(&w, "1");
    hooks::stop_after(None);
    assert_eq!(
        (code, &reply["seq"], &reply["handshake"]),
        (0, &json!(1), &Value::Null),
        "{reply}"
    );
    assert!(!exists(&w, "report"));
    client_json(&w);
    assert_eq!(
        gens::promote_ready(&w.ctx, KEY, ID, 1).unwrap(),
        gens::Ready::Undecided,
        "no report.json"
    );
    assert!(
        !exists(&w, "ready"),
        "the deadline decides (outcome-2), nothing is replayed"
    );
}

#[test]
fn killed_after_report_json_a_later_promote_ready_promotes() {
    let w = hc_world();
    pending(&w);
    hooks::stop_after(Some("report"));
    let (_, reply) = report_gen(&w, "1");
    hooks::stop_after(None);
    assert_eq!(
        (&reply["handshake"], &reply["ready"]),
        (&json!("reported"), &Value::Null),
        "{reply}"
    );
    assert!(w.fakes.slept().is_empty(), "stopped before the poll");
    assert!(exists(&w, "report") && !exists(&w, "ready"));
    client_json(&w);
    assert_eq!(
        gens::promote_ready(&w.ctx, KEY, ID, 1).unwrap(),
        gens::Ready::Ok
    );
    assert!(exists(&w, "ready"));
}

// After ready.json the verdict is ok; a second run (the hook again, or a replayed one) writes nothing.
#[test]
fn after_ready_json_nothing_more_is_written() {
    let w = hc_world();
    pending(&w);
    client_json(&w);
    assert_eq!(report_gen(&w, "1").1["ready"], "ok");
    let (report, ready) = (
        std::fs::read(gen_file(&w, "report")).unwrap(),
        std::fs::read(gen_file(&w, "ready")).unwrap(),
    );
    // (fix 1, audit 5-9 item 2) A later time: a second write of report.json would change its bytes.
    w.fakes.advance(Duration::from_secs(1));
    let slept = w.fakes.slept().len();
    let (_, reply) = report_gen(&w, "1");
    assert_eq!(
        (&reply["handshake"], &reply["ready"]),
        (&json!("reported"), &json!("ok"))
    );
    assert_eq!(
        std::fs::read(gen_file(&w, "report")).unwrap(),
        report,
        "report.json once"
    );
    assert_eq!(
        std::fs::read(gen_file(&w, "ready")).unwrap(),
        ready,
        "ready.json once"
    );
    assert_eq!(
        w.fakes.slept().len(),
        slept,
        "client.json is there: no sleep"
    );
}
