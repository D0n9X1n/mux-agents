//! prelife-1c fixtures: a team of lifecycle members, and pre-lifecycle folders beside it.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::agents::core_fixture as cf;
use crate::agents::fixture::World;
use crate::spawn::Out;

pub const L: &str = "111111111111";
pub const A: &str = "a0000000000a";
pub const C: &str = "c0000000000c";
pub const P: &str = "d0000000000d";
/// Task 9: an id that sorts after `C` and `P`, so holders named against their id order show the sort.
pub const D: &str = "e0000000000e";

/// Leader `L` -> lifecycle `A` -> pre-lifecycle `C` naming `A` as parent (spec "mixed tree"); A's and C's records.
pub fn mixed(w: &World) -> (Value, Value) {
    team(w);
    life(w, L, fields(L, "leader", None, 0));
    let a = life(w, A, fields(A, "w-a", Some(L), 1));
    let mut f = fields(C, "w-c", Some(A), 2);
    f["parentPane"] = json!("%6");
    (a, pre(w, C, f))
}

/// The hc verbs called so far (`hc_calls` argv[0] is the verb; `--version` included).
pub fn verbs(w: &World) -> Vec<String> {
    w.fakes
        .hc_calls()
        .iter()
        .map(|(_, a, _)| a[0].clone())
        .collect()
}

/// Whether any `hc kill` was called.
pub fn killed(w: &World) -> bool {
    verbs(w).iter().any(|v| v == "kill")
}

/// Every file under `dir` by relative path (P-R4); `gens/` included.
pub fn bytes(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(
                    p.strip_prefix(dir).unwrap().to_path_buf(),
                    std::fs::read(&p).unwrap(),
                );
            }
        }
    }
    out
}

/// Messages with delivery id `delivery` in pane `pane`'s mailbox file (read raw: `read_mail` would move a cursor).
pub fn delivered(w: &World, pane: &str, delivery: &str) -> usize {
    let key = crate::agents::mail::mailbox_key(pane, "").unwrap();
    let text = std::fs::read_to_string(w.state().join("mail").join(format!("{key}.jsonl")))
        .unwrap_or_default();
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|m| m["id"] == delivery)
        .count()
}

/// The team record of session $3, leader `L`.
pub fn team(w: &World) {
    use crate::agents::team as t;
    use crate::parse::Client;
    let made = t::Team {
        team: "t".into(),
        session: "$3".into(),
        name: "s".into(),
        directory: "/work".into(),
        client: Client::Claude,
        policy: t::policy(Client::Claude, &[]),
        flags: Vec::new(),
        leader: L.into(),
        created_at: "x".into(),
    };
    assert!(t::create(&t::path(&w.state(), "$3"), &made).unwrap());
}

/// A record's own fields over `cf::member`'s hc defaults; `parent` None is the leader (depth 0, `upTeam`).
pub fn fields(id: &str, name: &str, parent: Option<&str>, depth: u64) -> Value {
    let mut r = json!({"id": id, "name": name, "client": "claude", "session": "$3", "depth": depth,
        "directory": "/work", "pane": "%7", "sessionId": format!("{}-0000-4000-8000-{id}", &id[..8])});
    match parent {
        None => (r["upTeam"], r["parentAgent"], r["pane"]) = (json!("t"), Value::Null, json!("%4")),
        Some(p) => r["parentAgent"] = json!({"id": p}),
    }
    r
}

/// Generation `n`'s create-once `file` of member `id`.
pub fn publish(w: &World, id: &str, n: u64, file: &str, body: Value) {
    let path = crate::agents::gens::path(&w.folder(id), n, file);
    crate::state::evidence::publish(&path, &body).unwrap();
}

/// A lifecycle member at generation 1, committed (state-0 S2; `hcName` is `m` and the first 8 of the id), with
/// its generation's view so `subject_of` presents it.
pub fn life(w: &World, id: &str, mut f: Value) -> Value {
    let hc_name = format!("m{}", &id[..8]);
    (f["lifecycle"], f["generation"], f["hcName"]) = (json!(1), json!("1"), json!(hc_name));
    let r = cf::member(w, id, f);
    publish(w, id, 1, "commit", json!({"committed": true}));
    publish(w, id, 1, "launch", json!({"hcName": hc_name}));
    crate::agents::fixture::write(
        &w.folder(id).join("view.json"),
        json!({"pane": r["pane"], "window": "@5", "startCommand": r["startCommand"], "gen": 1,
            "hcAddress": r["hcAddress"], "hcName": hc_name}),
    );
    r
}

/// A pre-lifecycle hc member as main writes one: generation 0, `hcAddress`, `launcherArgv`, folder `commit.json`.
pub fn pre(w: &World, id: &str, f: Value) -> Value {
    cf::member(w, id, f)
}

/// `pre` with `launcherArgv` removed: hc fields but no usable identity (held, never looked up).
pub fn partial(w: &World, id: &str, f: Value) -> Value {
    let r = pre(w, id, f);
    let mut p = r.clone();
    p.as_object_mut().unwrap().remove("launcherArgv");
    crate::agents::fixture::write(&w.folder(id).join("record.json"), p);
    r
}

/// How hc answers `get <address>` in a scripted snapshot.
#[derive(Clone)]
pub enum Answer {
    Wire(Value),
    /// exit 1, "no such terminal": the terminal is gone from hc.
    Missing,
    /// the call fails (ambiguity 3: the core answered the listing, then this get failed).
    Fail,
}

/// The snapshot `for_spawn` reads: connect, the team listing (`listed` addresses), then each address in sorted
/// order (core_facts reads every listed and known address once).
pub fn script_listed(w: &World, listed: &[&str], answers: BTreeMap<String, Answer>) {
    cf::online(w);
    cf::listing(w, &crate::agents::membership::team(&w.caller()), listed);
    for (address, a) in answers {
        let argv = ["get", address.as_str(), "--json"];
        match a {
            Answer::Wire(v) => cf::get(w, &address, v),
            Answer::Missing => w.fakes.hc_expect(
                &argv,
                Ok(Out {
                    code: 1,
                    stderr: "no such terminal".into(),
                    ..Out::default()
                }),
            ),
            Answer::Fail => w.fakes.hc_expect(
                &argv,
                Err(crate::error::Refusal::internal("hc call failed")),
            ),
        }
    }
}

/// `script_listed` with an empty team listing.
pub fn script(w: &World, answers: BTreeMap<String, Answer>) {
    script_listed(w, &[], answers);
}

/// The team (leader `L`) and `children` Running lifecycle children `w1`..; their wires by address.
pub fn capacity_world_of(w: &World, children: u64) -> BTreeMap<String, Answer> {
    team(w);
    let mut out = BTreeMap::new();
    let leader = life(w, L, fields(L, "leader", None, 0));
    out.insert(format!("t{L}"), Answer::Wire(cf::wire(&leader)));
    for n in 1..=children {
        let id = format!("a{n}000000000{n}");
        let r = life(w, &id, fields(&id, &format!("w{n}"), Some(L), 1));
        out.insert(format!("t{id}"), Answer::Wire(cf::wire(&r)));
    }
    out
}

/// Five children: the team is one slot from `limit`.
pub fn capacity_world(w: &World) -> BTreeMap<String, Answer> {
    capacity_world_of(w, 5)
}

/// Spawn's team admission for a person, in spawn's order (spawn.rs:209-235): connect, team, admit, snapshot, place.
pub fn admit_spawn(w: &World) -> crate::error::Res<crate::agents::tree::Placed> {
    use crate::agents::tree;
    use crate::parse::Client;
    let caller = w.caller();
    let core = crate::hc::connect(&w.ctx)?;
    let found = tree::team_of(&w.ctx, &caller)?.expect("a team");
    let a = tree::admit(&w.ctx, &caller, &found, tree::Standing::Person)?;
    let s = crate::agents::membership::Snapshot::for_spawn(&w.ctx, &caller, &core)?;
    tree::place(&w.ctx, &caller, &a, &s, Client::Claude, None, || {
        s.name_only(None, Client::Claude, "add subtract")
    })
}

/// Makes `id`'s hc record (written by `cf::member`) a lifecycle generation-0 subject: `lifecycle: 1`, `options`
/// from its `spec.json` flags (else kept or empty), commit Run, `launch.json`, its view, and, when `ended`, the
/// recorded end (state-0 S6), so the record decides it Ended whatever hc says.
pub fn to_lifecycle(w: &World, id: &str, ended: bool) -> Value {
    let f = w.folder(id);
    let mut r: Value = crate::state::fs::read_json(&f.join("record.json"))
        .unwrap()
        .unwrap();
    let flags = crate::state::fs::read_json(&f.join("spec.json"))
        .ok()
        .flatten()
        .and_then(|s| s.get("flags").cloned());
    r["lifecycle"] = json!(1);
    if r.get("options").is_none() {
        r["options"] = flags.unwrap_or_else(|| json!([]));
    }
    crate::agents::fixture::write(&f.join("record.json"), r.clone());
    let name = format!("m{}", &id[..8]);
    for (file, body) in [
        ("commit", json!({"committed": true})),
        ("launch", json!({"hcName": name})),
    ] {
        let path = crate::agents::gens::path(&f, 0, file);
        if !path.exists() {
            crate::state::evidence::publish(&path, &body).unwrap();
        }
    }
    crate::agents::fixture::write(
        &f.join("view.json"),
        json!({"pane": r["pane"], "window": r["window"], "startCommand": r["startCommand"], "gen": 0,
            "hcAddress": r["hcAddress"], "hcName": name}),
    );
    let ended_path = crate::agents::gens::path(&f, 0, "ended");
    if ended && !ended_path.exists() {
        crate::state::evidence::publish(
            &ended_path,
            &json!({"state": "finished", "reason": "exited"}),
        )
        .unwrap();
    }
    r
}

/// v0.3-dev generation evidence in an unmarked folder (spec "Exclusion"): claim `run`, commit, launch, client.
pub fn dev_evidence(w: &World, id: &str) {
    let f = w.folder(id);
    std::fs::create_dir_all(f.join("gens")).unwrap();
    crate::agents::fixture::write(&crate::agents::gens::claim_path(&f, 1), json!("run"));
    publish(w, id, 1, "commit", json!({"committed": true}));
    publish(
        w,
        id,
        1,
        "launch",
        json!({"hcName": format!("m{}", &id[..8])}),
    );
    publish(w, id, 1, "client", json!({"pid": 4242, "start": 1500}));
}

/// Lifecycle `id`'s generation 1 recorded Ended (state-0 S6); in an unmarked folder, one more file a wrong reader
/// would open.
pub fn ended(w: &World, id: &str) {
    publish(
        w,
        id,
        1,
        "ended",
        json!({"state": "finished", "reason": "exited", "at": "x"}),
    );
}

/// Paths `record_reads` saw under `folder`, except its `record.json` (P-R1's one classifying read).
pub fn reads_past_record(reads: &[PathBuf], folder: &Path) -> Vec<PathBuf> {
    let record = folder.join("record.json");
    reads
        .iter()
        .filter(|p| p.starts_with(folder) && **p != record)
        .cloned()
        .collect()
}

/// What the multiplexer shows at a v0.2.8 member's recorded pane %9 (all calls 14).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pane028 {
    /// Live, started by the record's `startCommand`, pid 31337 (launcher.json's pid, listed with start 1000), until a
    /// `kill-pane` closes it.
    Live,
    /// Live, pid 31337, but started by another command: not proven the member's.
    Other,
    /// No pane at %9.
    Gone,
    /// `list-panes` fails: the multiplexer cannot be read.
    Unreachable,
}

/// Scripts `list-panes` for v0.2.8 member `id` as `pane` says, and its launcher's process row.
pub fn v028_pane(w: &World, id: &str, pane: Pane028) {
    use std::{cell::Cell, rc::Rc};
    w.fakes.process(crate::testing::row(
        31337,
        crate::agents::fixture::PANE,
        1000,
    ));
    let record: Value = crate::state::fs::read_json(&w.folder(id).join("record.json"))
        .unwrap()
        .unwrap();
    let command = match pane {
        Pane028::Other => "/bin/sh".to_string(),
        _ => record["startCommand"].as_str().unwrap().to_string(),
    };
    let closed = Rc::new(Cell::new(false));
    let shut = closed.clone();
    w.fakes.mux_script("kill-pane", move |_| {
        shut.set(true);
        Ok(crate::mux::Output::default())
    });
    let row = crate::agents::fixture::Listed {
        pane: "%9".into(),
        window: "@5".into(),
        pid: "31337".into(),
        dead: false,
        command,
        tag: id.into(),
        name: super::text(record.get("name")),
    };
    w.fakes.mux_script("list-panes", move |argv| {
        if pane == Pane028::Unreachable {
            return Ok(crate::mux::Output {
                code: 1,
                stdout: String::new(),
                stderr: "no server running".into(),
            });
        }
        let rows = match (pane, closed.get()) {
            (Pane028::Gone, _) | (_, true) => Vec::new(),
            _ => vec![row.clone()],
        };
        Ok(crate::mux::Output {
            code: 0,
            stdout: crate::agents::fixture::listed_rows(argv, &rows, "\n"),
            stderr: String::new(),
        })
    });
}

/// The `kill-pane` calls made so far (plain or inside a conditional `if-shell`).
pub fn pane_kills(w: &World) -> Vec<Vec<String>> {
    w.fakes
        .mux_calls()
        .into_iter()
        .filter(|c| c.iter().any(|a| a.starts_with("kill-pane")))
        .collect()
}

/// The kind of `id`'s folder notice, if any.
pub fn notice_kind(w: &World, id: &str) -> Option<String> {
    crate::state::fs::read_json(&w.folder(id).join("notice.json"))
        .ok()
        .flatten()
        .and_then(|n: Value| n["kind"].as_str().map(str::to_string))
}

/// How a v0.2.8 member ended, as its writers recorded it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum End {
    Live,
    Exited,
    Stopped,
    Lost,
}

/// Member `id` exactly as v0.2.8 wrote it (call 8; checklist 2): `record.json` (v0.2.8 spawn.rs:513-520),
/// `commit.json`, `spec.json` (spawn.rs:377-383: `schema`, `stateDirectory` = the server folder, `id`, `client`,
/// `parentPane`, the brief launch's `args` = `--session-id <uuid>` and the prompt, `prompt` = the tagged header
/// (spawn.rs:743-756), a blank line, the task), `startCommand` naming this member's own spec path (spawn.rs:425-427),
/// `launcher.json` (`pid`, `start`, `startedAt`; an exit adds `exitCode`, `exitedAt`) and, once ended, the folder
/// notice (`kind`, `exitCode` = the code for `exited` else null, `at`; Lost adds `closedBy: "pane"`). Stopped and
/// Lost launchers were killed before writing an exit.
pub fn v028(w: &World, id: &str, name: &str, end: End) {
    let f = w.folder(id);
    std::fs::create_dir_all(&f).unwrap();
    let write = crate::agents::fixture::write;
    let session = format!("{}-0000-4000-8000-{id}", &id[..8]);
    let task = "build it";
    let exe = "/bin/mux-agent";
    let header = [
        format!("[mux-agent] You are {name} (claude, id {id}), started in this tmux session."),
        "Your parent is root (%1); report to it with send parent; free text only in files.".into(),
        "Context: brief-only (human); brief: none; parent transcript: none".into(),
        format!("Reply: \"{exe}\" send parent -BodyFile <file>"),
        format!("Listen: \"{exe}\" read -Wait 600"),
    ];
    let prompt = format!("{}\n\n{task}", header.join("\n"));
    let state = w.state().to_string_lossy().replace('\\', "/");
    let spec = format!("{state}/agents/{id}/spec.json");
    write(
        &f.join("record.json"),
        json!({"schema": 1, "id": id, "name": name, "client": "claude", "sessionId": session,
            "context": "brief", "reason": "human", "session": "$3", "pane": "%9", "window": "@5",
            "parentPane": "%1", "parentAgent": null, "directory": "/work", "taskLine": task,
            "createdAt": "2026-11-01T00:00:00.0000000Z", "color": "blue",
            "startCommand": format!("{exe} launcher {spec}")}),
    );
    write(
        &f.join("commit.json"),
        json!({"committedAt": "2026-11-01T00:00:01.0000000Z"}),
    );
    write(
        &f.join("spec.json"),
        json!({"schema": 1, "stateDirectory": state, "id": id, "client": "claude", "parentPane": "%1",
            "args": ["--session-id", session, prompt], "prompt": prompt}),
    );
    let mut launcher =
        json!({"pid": 31337, "start": 1000, "startedAt": "2026-11-01T00:00:02.0000000Z"});
    if end == End::Exited {
        (launcher["exitCode"], launcher["exitedAt"]) =
            (json!(0), json!("2026-11-02T00:00:00.0000000Z"));
    }
    write(&f.join("launcher.json"), launcher);
    let at = "2026-11-02T00:00:01.0000000Z";
    let notice = match end {
        End::Live => None,
        End::Exited => Some(json!({"kind": "exited", "exitCode": 0, "at": at})),
        End::Stopped => Some(json!({"kind": "stopped", "exitCode": null, "at": at})),
        End::Lost => Some(json!({"kind": "lost", "exitCode": null, "at": at, "closedBy": "pane"})),
    };
    if let Some(n) = notice {
        write(&f.join("notice.json"), n);
    }
}
