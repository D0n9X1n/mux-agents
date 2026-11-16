//! report-2b K3, K3a, K4 through the real CLI (`cli::run`): fake proc, clock and hc; real files.
use super::core_fixture as cf;
use super::fixture::World;
use super::gens::path;
use super::session_report::{Input, Labels, hooks, labels, read_capped, read_input};
use crate::parse::Client;
use crate::state::evidence::publish;
use crate::state::live;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

const KEY: &str = "0123456789abcdef";
const ID: &str = "aaaaaaaaaaaa";
const NEW: &str = "0f11fc2f-2032-48ef-a920-4af2a8e5af50";
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

// (fix 2, re-audit #5) Through dispatch: partial or malformed labels, or an hc error, answer `handshake: "evidence"`
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

// (fix 2, re-audit #6) Bounded consumption: from an endless input the reader takes 64 KiB + 1 bytes and stops.
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

// (fix 2, re-audit #7) The production wrapper asks its input whether it is a terminal (`read_input`, which
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
