use super::*;
use crate::agents::{
    core_facts, core_identity,
    fixture::{World, write},
    membership::Stored,
    team as t,
};
use crate::agents::{core_fixture as cf, fixture::ID};
use crate::caller::Caller;
use crate::parse::Client;
use crate::proc::Row;
use serde_json::json;
use std::path::PathBuf;
pub(crate) const LEADER: &str = "111111111111";
pub(crate) fn team_record(w: &World, flags: &[&str]) {
    let flags: Vec<String> = flags.iter().map(|f| f.to_string()).collect();
    let made = t::Team {
        team: "t".into(),
        session: "$3".into(),
        name: "s".into(),
        directory: "/work".into(),
        client: Client::Claude,
        policy: t::policy(Client::Claude, &flags),
        flags,
        leader: LEADER.into(),
        created_at: "x".into(),
    };
    assert!(t::create(&t::path(&w.state(), "$3"), &made).unwrap());
}
pub(crate) fn leader_record() -> Value {
    json!({"id": LEADER, "name": "leader", "client": "claude", "session": "$3", "depth": 0, "upTeam": "t",
        "parentAgent": null, "pane": "%4"})
}
pub(crate) fn child(id: &str, parent: &str, depth: u64) -> Value {
    json!({"id": id, "name": format!("m{}", &id[10..]), "client": "claude", "session": "$3", "depth": depth, "directory": "/work",
        "parentAgent": {"id": parent}, "pane": "%7"})
}
pub(crate) fn snap(members: &[(&str, Value, Life)], reserved: usize) -> Snapshot {
    let (mut local, mut records) = (Vec::new(), BTreeMap::new());
    for (id, record, life) in members {
        let mut identity = core_identity::tests::identity();
        identity.id = id.to_string();
        local.push(core_facts::Observation {
            identity,
            terminal: None,
            life: *life,
        });
        records.insert(
            id.to_string(),
            Stored {
                record: record.clone(),
                committed: true,
                noticed: false,
            },
        );
    }
    let reserved = (0..reserved)
        .map(|n| (format!("t{n}"), format!("r{n}")))
        .collect();
    Snapshot {
        facts: core_facts::Snapshot { local, reserved },
        records,
        views: BTreeMap::new(),
        view_order: Vec::new(),
        core_error: None,
        view_error: None,
    }
}
pub(crate) fn ids(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("{:012x}", 0xa0 + i)).collect()
}
#[test]
fn scope_reads_the_team_record_or_a_launched_leader() {
    let w = World::human("");
    let c = w.caller();
    assert!(matches!(scope(&w.ctx, &c).unwrap(), Scope::Outside));
    let f = w.uncommitted(LEADER, json!({"depth": 0, "upTeam": "t"}));
    write(
        &f.join("abort.json"),
        json!({"runUncertain": false, "hcAddress": null}),
    );
    assert!(
        matches!(scope(&w.ctx, &c).unwrap(), Scope::Outside),
        "an abort that ran nothing is no leader"
    );
    std::fs::remove_file(f.join("abort.json")).unwrap();
    assert!(
        matches!(scope(&w.ctx, &c).unwrap(), Scope::Leaderless),
        "a crash before the team record"
    );
    team_record(&w, &[]);
    assert!(matches!(scope(&w.ctx, &c).unwrap(), Scope::Team(_)));
}
#[test]
fn the_limit_counts_live_and_pending_members_besides_the_leader() {
    let check = |n: usize, life: Life, reserved: usize| {
        let mut rows = vec![(LEADER.to_string(), leader_record(), Life::Running)];
        rows.extend(ids(n).into_iter().map(|id| {
            let r = child(&id, LEADER, 1);
            (id, r, life)
        }));
        let rows: Vec<_> = rows
            .iter()
            .map(|(i, r, l)| (i.as_str(), r.clone(), *l))
            .collect();
        limit(&snap(&rows, reserved))
    };
    assert!(check(5, Life::Running, 0).is_ok());
    let full = refuse::<()>(
        2,
        "limit",
        "the team already has 6 members besides the leader",
    );
    assert_eq!(check(6, Life::Running, 0), full);
    assert_eq!(check(5, Life::Running, 1), full, "a pending member counts");
    assert!(check(6, Life::Ended, 0).is_ok());
    assert!(check_depth(3).is_ok());
    assert_eq!(
        check_depth(4),
        refuse(
            2,
            "depth",
            "a child at depth 4 exceeds the team's depth limit of 3"
        )
    );
}
pub(crate) const OTHER: &str = "bbbbbbbbbbbb";
/// Paths are cached; `reparent()` (also run by `hc()`) comes after every caller-resolving fixture: caller 4242 > shell 170 > claude C 150 >
/// launcher L 31337 for ID (depth 1); OTHER (depth 1) records L 41000, C 151.
pub(crate) struct Mw {
    pub w: World,
    pub c: Caller,
    pub state: PathBuf,
    pub r: Value,
}
impl Mw {
    pub fn folder(&self, id: &str) -> PathBuf {
        self.state.join("agents").join(id)
    }
    /// ID's C (150) under its launcher L (31337). After it `World::caller()` no longer reaches the pane, so every
    /// fixture that resolves the caller (`cf::member`, `w.state()`) is made before it; later writes use `folder`.
    pub fn reparent(&self) {
        self.w.fakes.forget(150);
        self.w.fakes.process(Row {
            pid: 150,
            ppid: 31337,
            start: Some(1500),
            path: None,
            argv: Some(vec!["claude".into()]),
        });
        self.w.fakes.process(crate::testing::row(31337, 1, 1000));
    }
    pub fn hc(&mut self) {
        self.reparent();
        self.w.ctx.env = crate::ctx::Env::from_pairs([(
            "HC_TERMINAL_ID",
            self.r["hcAddress"].as_str().unwrap(),
        )]);
    }
    /// `n` rounds of connect and a get of ID's terminal `t`.
    pub fn rounds(&self, n: usize, t: &Value) {
        for _ in 0..n {
            cf::online(&self.w);
            cf::get(&self.w, self.r["hcAddress"].as_str().unwrap(), t.clone());
        }
    }
    pub fn record(&self, id: &str) -> Value {
        crate::state::fs::read_json(&self.folder(id).join("record.json"))
            .unwrap()
            .unwrap()
    }
}
pub(crate) fn member_world() -> Mw {
    let w = World::agent("");
    let (c, state) = (w.caller(), w.state());
    write(
        &state.join("server.json"),
        json!({"socket": c.socket, "serverPid": c.server_pid, "serverStart": c.server_start}),
    );
    team_record(&w, &[]);
    cf::member(&w, LEADER, leader_record());
    let r = cf::member(&w, ID, child(ID, LEADER, 1));
    cf::member(&w, OTHER, child(OTHER, LEADER, 1));
    cf::proven(&w, ID, 31337, 150);
    cf::proven(&w, OTHER, 41000, 151);
    Mw { w, c, state, r }
}
pub(crate) fn term(r: &Value, foreground: Option<i32>) -> Value {
    let mut t = cf::wire(r);
    t["pid"] = 31337.into();
    if let Some(pid) = foreground {
        t["foreground"] = json!({"argv": ["claude"], "pid": pid});
    }
    t
}
/// `prove_member` over member `id`'s terminal (pid 31337) with foreground `fg`; no caller resolution.
fn prove(m: &Mw, id: &str, fg: Option<i32>) -> Option<Member> {
    let r = m.record(id);
    let address = r["hcAddress"].as_str().unwrap();
    cf::online(&m.w);
    cf::get(&m.w, address, term(&r, fg));
    let t = crate::hc::connect(&m.w.ctx).unwrap().get(address).unwrap();
    prove_member(
        &m.w.ctx,
        &m.state,
        &crate::agents::membership::team(&m.c),
        id,
        &t,
    )
}
#[test]
fn a_member_is_proven_by_its_recorded_launcher_and_client() {
    for fg in [150, 31337] {
        let m = member_world();
        m.reparent();
        assert!(
            matches!(prove(&m, ID, Some(fg)), Some(x) if x.pid == 150 && x.flags == ["--effort", "high"]),
            "{fg}"
        );
        m.w.fakes.hc_done().unwrap();
    }
    for case in [
        "nested",
        "non-shell",
        "c-prime",
        "l-pid",
        "l-start",
        "c-pid",
        "c-start",
        "other-member",
        "no-foreground",
        "foreground",
        "unreadable",
    ] {
        let m = member_world();
        m.reparent();
        let (mut fg, mut id, lj) = (Some(150), ID, m.folder(ID).join("launcher.json"));
        let rec = |l: u32, ls: i64, c: u32, cs: i64| json!({"pid": l, "start": ls, "client": {"pid": c, "start": cs}});
        match case {
            "nested" | "non-shell" | "c-prime" => {
                m.w.fakes.forget(170);
                m.w.fakes.process(crate::testing::row(170, 160, 1700));
                m.w.fakes.process(crate::testing::row(
                    160,
                    if case == "c-prime" { 31337 } else { 150 },
                    1600,
                ));
                if case != "non-shell" {
                    m.w.fakes.client(160, Client::Claude);
                }
            }
            "l-pid" => write(&lj, rec(31338, 1000, 150, 1500)),
            "l-start" => write(&lj, rec(31337, 999, 150, 1500)),
            "c-pid" => write(&lj, rec(31337, 1000, 152, 1500)),
            "c-start" => write(&lj, rec(31337, 1000, 150, 1499)),
            "other-member" => id = OTHER,
            "no-foreground" => fg = None,
            "foreground" => fg = Some(170),
            _ => m.w.fakes.forget(170),
        }
        assert_eq!(prove(&m, id, fg), None, "{case}");
        m.w.fakes.hc_done().unwrap();
    }
}
fn row(pid: u32, ppid: u32, start: i64, path: &str, argv: &[&str]) -> Row {
    Row {
        pid,
        ppid,
        start: Some(start),
        path: Some(path.into()),
        argv: Some(argv.iter().map(|a| a.to_string()).collect()),
    }
}
#[test]
fn the_copilot_pair_is_unix_loader_to_native() {
    use crate::state::root::OsKind::{Unix, Windows};
    // The canonical Unix recognition (`parse::unix_client`, as parse.rs:853-940 tests it): links resolved, a relative
    // script joined to the process cwd (/p), an unresolvable path no client.
    let canon = |r: &Row| {
        let realpath = |p: &str| match p {
            "/usr/local/bin/copilot" | "/p/bin/copilot" => {
                Some("/usr/local/lib/node_modules/@github/copilot/npm-loader.js".to_string())
            }
            "/x/@github/copilot/npm-loader.js" => Some("/srv/app.js".to_string()),
            "/broken" => None,
            other => Some(other.to_string()),
        };
        crate::parse::unix_client(
            r.path.as_deref(),
            r.argv.as_deref(),
            || Some("/p".into()),
            realpath,
        ) == Some(Client::Copilot)
    };
    let native = row(155, 150, 1550, "/opt/copilot/copilot", &["copilot"]);
    let loader = |script: &str| row(150, 31337, 1500, "/usr/bin/node", &["node", script]);
    for script in ["/usr/local/bin/copilot", "bin/copilot"] {
        assert!(
            copilot_pair(&loader(script), &native, Unix, canon),
            "{script}: a link or a relative invocation"
        );
    }
    assert!(
        !copilot_pair(&loader("/usr/local/bin/copilot"), &native, Windows, canon),
        "Unix only"
    );
    assert!(
        !copilot_pair(
            &loader("/x/@github/copilot/npm-loader.js"),
            &native,
            Unix,
            canon
        ),
        "a loader-shaped link to another script"
    );
    assert!(
        !copilot_pair(&loader("/broken"), &native, Unix, canon),
        "unreadable resolution"
    );
    assert!(
        !copilot_pair(
            &row(150, 31337, 1500, "/opt/copilot/copilot", &["copilot"]),
            &native,
            Unix,
            canon
        ),
        "native to native"
    );
    assert!(
        !copilot_pair(
            &loader("bin/copilot"),
            &row(155, 150, 1550, "/usr/bin/node", &["node", "bin/copilot"]),
            Unix,
            canon
        ),
        "loader to loader"
    );
    if crate::state::root::HOST == Unix {
        // Loader start 1500 is C's start as `cf::proven` records it; "c-start" above keeps the mismatch case.
        let mut m = member_world();
        m.r["client"] = "copilot".into();
        write(&m.folder(ID).join("record.json"), m.r.clone());
        m.reparent();
        m.w.fakes.forget(150);
        m.w.fakes.process(loader("/usr/local/bin/copilot"));
        m.w.fakes.process(native.clone());
        for pid in [150, 155] {
            m.w.fakes.client(pid, Client::Copilot);
        }
        m.w.fakes.forget(170);
        m.w.fakes.process(crate::testing::row(170, 155, 1700));
        assert!(
            matches!(prove(&m, ID, Some(155)), Some(x) if x.pid == 150 && x.client == Client::Copilot),
            "the pair"
        );
        m.w.fakes.hc_done().unwrap();
    }
}
