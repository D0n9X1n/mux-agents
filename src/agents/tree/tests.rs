use super::*;
use crate::agents::cascade::Tree;
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
use serde_json::Map;
use serde_json::json;
use std::path::PathBuf;
#[cfg(test)]
mod role_tests;
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
                notice: None,
                active_done: None,
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
        held: Vec::new(),
        pre_ended: Default::default(),
        pre: Default::default(),
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
    assert!(check(MAX_MEMBERS - 1, Life::Running, 0).is_ok());
    let full = refuse::<()>(
        2,
        "limit",
        format!("the team already has {MAX_MEMBERS} members besides the leader"),
    );
    assert_eq!(check(MAX_MEMBERS, Life::Running, 0), full);
    assert_eq!(
        check(MAX_MEMBERS - 1, Life::Running, 1),
        full,
        "a pending member counts"
    );
    assert!(check(MAX_MEMBERS, Life::Ended, 0).is_ok());
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
            image: None,
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
    prove_member(&m.w.ctx, &crate::agents::membership::team(&m.c), id, &r, &t)
}
/// K2 fixtures: ID made a lifecycle record of generation `g` (stored options `--model x`; an `hcName` beyond 0, as
/// A2a Task 4 requires), `reparent()` done, and the folder-level `launcher.json` removed so that only generation
/// evidence can prove it. The folder-level `spec.json` (`--effort high`) stays, so flags read from it would show.
fn lifecycle_world(g: u64) -> Mw {
    let m = member_world();
    m.reparent();
    let mut r = m.record(ID);
    (r["lifecycle"], r["options"], r["generation"]) =
        (json!(1), json!(["--model", "x"]), json!(g.to_string()));
    if g > 0 {
        r["hcName"] = json!("m0123abcd");
    }
    write(&m.folder(ID).join("record.json"), r);
    std::fs::remove_file(m.folder(ID).join("launcher.json")).unwrap();
    m
}
/// Generation `n`'s `launcher.json` (L 31337, start 1000) and `client.json` (C 150, start 1500), published.
fn evidence(m: &Mw, n: u64) {
    use crate::{agents::gens::path, state::evidence::publish};
    let f = m.folder(ID);
    publish(
        &path(&f, n, "launcher"),
        &json!({"pid": 31337, "start": 1000, "startedAt": "x"}),
    )
    .unwrap();
    publish(&path(&f, n, "client"), &json!({"pid": 150, "start": 1500})).unwrap();
}
/// Generation `n`'s `commit.json`, published.
fn commit(m: &Mw, n: u64, body: Value) {
    crate::state::evidence::publish(
        &crate::agents::gens::path(&m.folder(ID), n, "commit"),
        &body,
    )
    .unwrap();
}
/// `prove_member` over member `id` (the raw string) with the record value `r` passed in, foreground 150; answers the
/// proof and every path `read_json` was asked to open during the proof call alone.
fn prove_reads(m: &Mw, id: &str, r: &Value) -> (Option<Member>, Vec<PathBuf>) {
    let address = r["hcAddress"].as_str().unwrap();
    cf::online(&m.w);
    cf::get(&m.w, address, term(r, Some(150)));
    let t = crate::hc::connect(&m.w.ctx).unwrap().get(address).unwrap();
    let team = crate::agents::membership::team(&m.c);
    crate::state::fs::record_reads(|| prove_member(&m.w.ctx, &team, id, r, &t))
}
// H6: member proof rests on labels and generation evidence; no registry entry is read. With no entry, one naming
// another conversation, and a torn one, the member is proven and no path under `live/` is read (evidence reads and
// file opens both recorded).
#[test]
fn caller_proof_reads_no_registry_entry() {
    for case in ["none", "other", "torn"] {
        let m = lifecycle_world(1);
        evidence(&m, 1);
        commit(&m, 1, json!({"committed": true}));
        crate::testing::seed_entry(&m.w.ctx, case);
        let live = crate::state::live::dir(&m.w.ctx);
        let ((proven, opened), asked) = crate::state::evidence::record_evidence_reads(|| {
            crate::state::fs::record_reads(|| prove(&m, ID, Some(150)))
        });
        assert!(proven.is_some(), "{case}");
        assert!(
            !asked
                .iter()
                .chain(opened.iter())
                .any(|p| p.starts_with(&live)),
            "{case}: {asked:?} {opened:?}"
        );
    }
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
        image: None,
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
#[test]
fn standing_proves_under_the_lock_and_keeps_a_pane_agent_caller() {
    let mut m = member_world();
    m.hc();
    let mut t = term(&m.r, Some(150));
    t.as_object_mut().unwrap().remove("pid");
    m.rounds(2, &t);
    let c = crate::caller::resolve_tree(&m.w.ctx).unwrap();
    assert_eq!(
        c.candidate.as_deref(),
        Some(ID),
        "the walk cannot run without a terminal pid"
    );
    assert!(
        matches!(standing(&m.w.ctx, &c, None), Ok(Standing::Member(x)) if x.id == ID),
        "proven under the lock"
    );
    m.w.fakes.forget(170);
    m.rounds(2, &term(&m.r, Some(150)));
    let c = crate::caller::resolve_tree(&m.w.ctx).unwrap();
    assert_eq!(
        standing(&m.w.ctx, &c, None),
        Err(tagged()),
        "a failed proof never falls back"
    );
    m.w.fakes.hc_done().unwrap();
    let w = World::human("");
    assert_eq!(standing(&w.ctx, &w.caller(), None), Ok(Standing::Person));
    let h = World::human(ID);
    assert_eq!(
        standing(&h.ctx, &h.caller(), None),
        Err(tagged()),
        "a tagged pane"
    );
    let a = World::agent("");
    assert_eq!(
        standing(&a.ctx, &a.caller(), None).unwrap_err().check,
        "caller",
        "a tmux-pane agent keeps 2 caller"
    );
}
pub(crate) fn member_of(id: &str, record: Value) -> Standing {
    Standing::Member(Member {
        id: id.into(),
        record,
        client: Client::Claude,
        pid: 150,
        start: 1500,
        flags: vec![],
    })
}
#[test]
fn admission_runs_in_v5_order() {
    let w = World::human("");
    let c = w.caller();
    assert_eq!(team_of(&w.ctx, &c), Ok(None), "outside a team, 0.2");
    w.uncommitted(LEADER, json!({"depth": 0, "upTeam": "t"}));
    assert_eq!(
        team_of(&w.ctx, &c),
        no_team(),
        "a leader without its record refuses before any hc call"
    );
    std::fs::remove_dir_all(w.folder(LEADER)).unwrap();
    team_record(&w, &[]);
    let team = team_of(&w.ctx, &c).unwrap().unwrap();
    // A full team: a chain leader -> i0 -> i1 -> i2 (depths 1-3), every other member a child of the leader.
    let i = ids(MAX_MEMBERS);
    cf::member(&w, LEADER, leader_record());
    for (n, id) in i.iter().enumerate() {
        let parent = if (1..3).contains(&n) {
            i[n - 1].as_str()
        } else {
            LEADER
        };
        cf::member(
            &w,
            id,
            child(id, parent, if n < 3 { n as u64 + 1 } else { 1 }),
        );
    }
    let rec = |id: &str| {
        crate::state::fs::read_json(&w.folder(id).join("record.json"))
            .unwrap()
            .unwrap()
    };
    assert_eq!(
        admit(&w.ctx, &c, &team, member_of(&i[2], rec(&i[2])))
            .unwrap_err()
            .check,
        "depth",
        "before any snapshot"
    );
    let a = admit(&w.ctx, &c, &team, member_of(&i[1], rec(&i[1]))).unwrap();
    let mut rows: Vec<_> = std::iter::once(LEADER)
        .chain(i.iter().map(String::as_str))
        .map(|id| (id, rec(id), Life::Running))
        .collect();
    let kid = || -> Res<String> { Ok("kid".into()) };
    assert_eq!(
        place(&w.ctx, &c, &a, &snap(&rows, 0), Client::Claude, None, kid)
            .unwrap_err()
            .check,
        "limit"
    );
    rows.pop();
    let (short, budget) = (
        snap(&rows, 0),
        crate::agents::budget::path(&w.state(), "$3"),
    );
    let taken = place(&w.ctx, &c, &a, &short, Client::Claude, None, || {
        refuse(2, "name", "taken")
    });
    assert_eq!(taken.unwrap_err().check, "name");
    assert!(!budget.exists(), "a name refusal spends no token");
    write(&budget, json!({"tokens": 0, "at": 0}));
    assert_eq!(
        place(&w.ctx, &c, &a, &short, Client::Claude, None, kid)
            .unwrap_err()
            .check,
        "budget",
        "budget last"
    );
    let pa = admit(&w.ctx, &c, &team, Standing::Person).unwrap();
    let p = place(&w.ctx, &c, &pa, &short, Client::Claude, None, kid).unwrap();
    assert_eq!(
        (
            p.depth,
            &p.parent["id"],
            p.parent_pane.as_str(),
            p.flags.as_slice(),
            p.name.as_str()
        ),
        (
            1,
            &json!(LEADER),
            "%4",
            &["--effort".to_string(), "high".into()][..],
            "kid"
        ),
        "a person's child: leader, free"
    );
    std::fs::remove_file(&budget).unwrap();
    let m = place(&w.ctx, &c, &a, &short, Client::Claude, None, kid).unwrap();
    assert_eq!(
        (m.depth, &m.parent["id"], m.directory.as_deref()),
        (3, &json!(i[1]), Some("/work"))
    );
    assert!(
        !budget.exists(),
        "place checks the budget; the launch spends it (D5)"
    );
}
#[test]
fn ancestors_end_at_the_leader() {
    let i = ids(3);
    let mut r: BTreeMap<String, Value> = [
        (LEADER.to_string(), leader_record()),
        (i[0].clone(), child(&i[0], LEADER, 1)),
        (i[1].clone(), child(&i[1], &i[0], 2)),
    ]
    .into();
    assert_eq!(
        Tree::from_records(&r).ancestors(&i[1], LEADER).unwrap(),
        [i[0].clone(), LEADER.to_string()]
    );
    assert!(
        Tree::from_records(&r)
            .ancestors(LEADER, LEADER)
            .unwrap()
            .is_empty()
    );
    r.insert(i[2].clone(), child(&i[2], "cccccccccccc", 2));
    assert_eq!(
        Tree::from_records(&r).ancestors(&i[2], LEADER),
        refuse(2, "authority", "the parent chain of ma2 cannot be read"),
        "broken"
    );
    for (id, bad) in [
        (&i[0], child(&i[0], &i[1], 1)),
        (&i[0], json!({"depth": 0, "parentAgent": null})),
        (&i[0], child(&i[0], LEADER, 2)),
    ] {
        let mut r = r.clone();
        r.insert(id.clone(), bad);
        assert_eq!(
            Tree::from_records(&r)
                .ancestors(&i[1], LEADER)
                .unwrap_err()
                .check,
            "authority",
            "cycle, non-leader root, depth"
        );
    }
}
#[test]
fn a_child_inherits_the_lower_of_its_parent_and_the_ceiling() {
    use t::Policy::{AllowAll, Bypass, Default as Def};
    let f = |c, p, ceiling| child_flags(c, p, ceiling, None).unwrap();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(
        f(Client::Claude, Def, Def),
        s(&["--effort", "high"]),
        "a default leader's child: no bypass"
    );
    assert_eq!(
        f(Client::Claude, Def, Bypass),
        s(&["--effort", "high"]),
        "parent below the ceiling"
    );
    assert_eq!(
        f(Client::Claude, Bypass, Def),
        s(&["--effort", "high"]),
        "parent above the ceiling"
    );
    assert_eq!(
        f(Client::Claude, Bypass, Bypass),
        s(&["--dangerously-skip-permissions", "--effort", "high"])
    );
    assert_eq!(
        f(Client::Claude, AllowAll, Bypass),
        s(&["--effort", "high"]),
        "Copilot AllowAll parent: Claude Default"
    );
    assert_eq!(
        f(Client::Copilot, Bypass, Bypass),
        s(&["--allow-all", "--no-sandbox"]),
        "Claude Bypass: Copilot AllowAll"
    );
    assert_eq!(
        bound(Bypass, Def, Bypass),
        refuse(2, "ceiling", "the child's flags exceed its parent's policy")
    );
    assert_eq!(
        bound(Bypass, Bypass, Def),
        refuse(
            2,
            "ceiling",
            "the child's flags exceed the team's permission ceiling"
        )
    );
}
#[test]
fn of_two_racing_spawns_at_the_limit_one_wins() {
    let w = World::human("");
    team_record(&w, &[]);
    cf::member(&w, LEADER, leader_record());
    // One slot short of the limit: the first spawn fills it, the second is refused.
    for id in ids(MAX_MEMBERS - 1) {
        cf::member(&w, &id, child(&id, LEADER, 1));
    }
    let (state, locks) = (
        w.ctx.state_root.clone(),
        w.ctx.state_root.parent().unwrap().join("locks"),
    );
    let key = crate::agents::lock_key("spawn", &w.caller(), "$3");
    let race = |n: usize| {
        let mut r = World::human("");
        r.ctx.state_root = state.clone();
        r.fakes.real_locks(&locks);
        let c = r.caller();
        crate::agents::lock(&r.ctx, &key, || {
            let a = admit(&r.ctx, &c, &team_of(&r.ctx, &c)?.unwrap(), Standing::Person)?;
            let mut all = Vec::new();
            for (f, m) in crate::agents::resume::every_member(&r.ctx)? {
                match m {
                    crate::agents::gens::Member::Pre(v) => all.push((f, v)),
                    crate::agents::gens::Member::Life(g) => all.push((f, g.record().clone())),
                    crate::agents::gens::Member::Absent => {}
                    crate::agents::gens::Member::Unreadable(e) => return Err(e),
                }
            }
            let rows: Vec<_> = all
                .iter()
                .map(|(f, v)| {
                    (
                        f.file_name().unwrap().to_str().unwrap(),
                        v.clone(),
                        Life::Running,
                    )
                })
                .collect();
            place(
                &r.ctx,
                &c,
                &a,
                &snap(&rows, 0),
                Client::Claude,
                None,
                || Ok(format!("w{n}")),
            )?;
            let id = format!("{:012x}", 0xf0 + n);
            r.uncommitted(&id, child(&id, LEADER, 1));
            Ok(())
        })
        .is_ok()
    };
    let wins = std::thread::scope(|s| {
        let all: Vec<_> = (0..2).map(|n| s.spawn(move || race(n))).collect();
        all.into_iter()
            .map(|h| h.join().unwrap())
            .filter(|won| *won)
            .count()
    });
    assert_eq!(wins, 1);
}
#[test]
fn a_candidate_spawn_is_proven_under_the_lock_or_refused_with_no_side_effects() {
    let args = ["spawn", "-Client", "claude", "-TaskFile", "t.md"].map(String::from);
    // Refused: walk and proof fail. OTHER is ended in hc but never reconciled: no `ls`, notice, mail, budget or run.
    let mut m = member_world();
    m.w.file("t.md", b"task");
    m.w.fakes.forget(170);
    m.hc();
    m.rounds(3, &term(&m.r, Some(150)));
    let (code, out) = crate::cli::run(&args, || Ok(&m.w.ctx));
    assert_eq!(
        (code, out.contains(r#""check":"tagged""#)),
        (2, true),
        "{out}"
    );
    assert!(
        !m.folder(OTHER).join("notice.json").exists()
            && !crate::agents::budget::path(&m.state, "$3").exists()
    );
    assert!(
        !m.w.fakes
            .hc_calls()
            .iter()
            .any(|c| c.1.first().is_some_and(|a| a == "ls" || a == "run")),
        "no reconcile, no launch"
    );
    m.w.fakes.hc_done().unwrap();
    // Proven only under the lock: launcher.json gains C when the team lock is taken; ID at depth 3 then refuses `depth`.
    let mut m = member_world();
    m.w.file("t.md", b"task");
    let (p1, p2) = ("cccccccccc01", "cccccccccc02");
    cf::member(&m.w, p1, child(p1, LEADER, 1));
    cf::member(&m.w, p2, child(p2, p1, 2));
    let mut r = m.record(ID);
    (r["depth"], r["parentAgent"]) = (json!(3), json!({"id": p2}));
    write(&m.folder(ID).join("record.json"), r);
    let lj = m.folder(ID).join("launcher.json");
    write(&lj, json!({"pid": 31337, "start": 1000}));
    let plus = format!("+{}", crate::agents::lock_key("spawn", &m.c, "$3"));
    m.w.fakes.lock_probe(move |e| {
        if e == plus {
            write(
                &lj,
                json!({"pid": 31337, "start": 1000, "client": {"pid": 150, "start": 1500}}),
            );
        }
        String::new()
    });
    m.hc();
    m.rounds(3, &term(&m.r, Some(150)));
    let (code, out) = crate::cli::run(&args, || Ok(&m.w.ctx));
    assert_eq!(
        (code, out.contains(r#""check":"depth""#)),
        (2, true),
        "proven under the lock: {out}"
    );
    m.w.fakes.hc_done().unwrap();
}
#[test]
fn a_leaderless_team_refuses_before_hc() {
    let w = World::human("");
    w.uncommitted(LEADER, json!({"depth": 0, "upTeam": "t"}));
    let e = crate::agents::spawn::spawn(
        &w.ctx,
        &w.request(&["spawn", "-Client", "claude", "-Task", "hi"]),
    )
    .unwrap_err();
    assert_eq!((e.code, e.check), (1, "team"));
    assert!(w.fakes.hc_calls().is_empty(), "hc is not even installed");
}
#[test]
fn authority_runs_only_down_the_tree() {
    // i0 and i2 under the leader; i1 and i4 under i0; i3 under i2. Target i1 (`ma1`).
    let i = ids(5);
    let mut r: BTreeMap<String, Value> = [
        (LEADER.to_string(), leader_record()),
        (i[0].clone(), child(&i[0], LEADER, 1)),
        (i[1].clone(), child(&i[1], &i[0], 2)),
        (i[2].clone(), child(&i[2], LEADER, 1)),
        (i[3].clone(), child(&i[3], &i[2], 2)),
        (i[4].clone(), child(&i[4], &i[0], 2)),
    ]
    .into();
    let m = |id: &str, r: &BTreeMap<String, Value>| member_of(id, r[id].clone());
    for who in [LEADER, i[0].as_str()] {
        assert!(
            authorize(&Tree::from_records(&r), LEADER, &m(who, &r), &i[1], "stop").is_ok(),
            "{who}: an ancestor"
        );
    }
    assert!(
        authorize(
            &Tree::from_records(&r),
            LEADER,
            &Standing::Person,
            &i[1],
            "read"
        )
        .is_ok(),
        "a person"
    );
    for who in [&i[4], &i[2], &i[3], &i[1]] {
        assert_eq!(
            authorize(&Tree::from_records(&r), LEADER, &m(who, &r), &i[1], "read"),
            refuse(2, "authority", "only an ancestor or a person can read ma1"),
            "{who}: sibling, peer, cousin, self"
        );
    }
    assert_eq!(
        authorize(
            &Tree::from_records(&r),
            LEADER,
            &m(&i[1], &r),
            &i[0],
            "stop"
        )
        .unwrap_err()
        .check,
        "authority",
        "a child"
    );
    r.insert(i[1].clone(), child(&i[1], "cccccccccccc", 2));
    assert_eq!(
        authorize(
            &Tree::from_records(&r),
            LEADER,
            &m(&i[0], &r),
            &i[1],
            "stop"
        ),
        refuse(2, "authority", "the parent chain of ma1 cannot be read"),
        "a broken chain"
    );
}
#[test]
fn a_sibling_cannot_stop_or_read_and_mail_grants_nothing() {
    for verb in ["stop", "read"] {
        let mut m = member_world();
        crate::agents::mail::add_mail(&m.w.ctx, &m.c, ID, "leader", "%4", "please stop mbb", None)
            .unwrap();
        m.hc();
        m.rounds(2, &term(&m.r, Some(150)));
        let (def, request) = crate::cli::parse(&[verb, "mbb"].map(String::from)).unwrap();
        let e = (def.handler)(&m.w.ctx, &request).unwrap_err();
        assert_eq!(
            (e.code, e.check, e.message),
            (
                2,
                "authority",
                format!("only an ancestor or a person can {verb} mbb")
            )
        );
        assert!(!m.folder(OTHER).join("notice.json").exists(), "{verb}");
        m.w.fakes.hc_done().unwrap();
    }
}
#[test]
fn a_candidate_is_proven_under_the_team_lock_or_refused() {
    let mut m = member_world();
    m.w.fakes.forget(170);
    m.hc();
    m.rounds(3, &term(&m.r, Some(150)));
    let (code, out) = crate::cli::run(&["stop", "mbb"].map(String::from), || Ok(&m.w.ctx));
    assert_eq!(
        (code, out.contains(r#""check":"tagged""#)),
        (2, true),
        "{out}"
    );
    assert!(
        m.w.fakes.locks().contains(&format!(
            "+{}",
            crate::agents::lock_key("spawn", &m.c, "$3")
        )),
        "under the lock"
    );
    assert!(!m.folder(OTHER).join("notice.json").exists());
    m.w.fakes.hc_done().unwrap();
}
/// hc as resume reads it under the lock: the listing, then each terminal by address (X exited).
fn facts_of(w: &World, team: &str, rows: &[Value], x: &Value) {
    cf::listing(w, team, &[]);
    let mut ended = cf::wire(x);
    (ended["state"], ended["reason"]) = (json!("finished"), json!("exited"));
    let mut all: Vec<(String, Value)> = rows
        .iter()
        .map(|r| (r["hcAddress"].as_str().unwrap().into(), cf::wire(r)))
        .collect();
    all.push((x["hcAddress"].as_str().unwrap().into(), ended));
    all.sort_by(|a, b| a.0.cmp(&b.0));
    for (address, wire) in all {
        cf::get(w, &address, wire);
    }
}
fn launch_ok(w: &World) {
    w.fakes.hc_answer(Ok(crate::spawn::Out {
        stdout: r#"{"address":"t9"}"#.into(),
        ..Default::default()
    }));
    for verb in ["new-window", "split-window"] {
        w.fakes.mux_rule(verb, 0, "%9|@5|exact,#} attach t9\n", "");
    }
    w.launches_hold();
    w.fakes
        .mux_rule("-t %9 #{pane_id}|#{@agent-id}", 0, "|\n", "");
}
type Held = std::rc::Rc<std::cell::RefCell<Vec<bool>>>;
/// `resume_case`'s predecessor A of ID, and A's second successor.
const PRED: &str = "eeeeeeeeeeee";
const S2: &str = "ffffffffffff";
/// `held`: whether the team lock was held at each `hc get` (recorded in the "candidate" case, run through the CLI).
fn resume_case(case: &str) -> (Mw, Res<Map<String, Value>>, String, Held) {
    let mut m = member_world();
    let x = "dddddddddddd".to_string();
    // Checklist 14: "successor": ID resumed A (PRED), X is A's child; "successors": S2 resumed A too.
    let succ = case == "successor" || case == "successors";
    let parent = match case {
        "non-descendant" => OTHER,
        _ if succ => PRED,
        _ => ID,
    };
    let mut rx = child(&x, parent, 2);
    rx["sessionId"] = "22222222-2222-4222-8222-222222222222".into();
    let rx = cf::member(&m.w, &x, rx);
    let mut extra = Vec::new();
    if succ {
        extra.push(cf::member(&m.w, PRED, child(PRED, LEADER, 1)));
        m.r["resumedFrom"] = json!(PRED);
        write(&m.folder(ID).join("record.json"), m.r.clone());
    }
    if case == "successors" {
        let mut s2 = child(S2, LEADER, 1);
        s2["resumedFrom"] = json!(PRED);
        extra.push(cf::member(&m.w, S2, s2));
    }
    if case == "no-directory" {
        // A record whose run input cannot be built: no `directory` for `hc run --cwd`.
        let mut r = m.record(&x);
        r.as_object_mut().unwrap().remove("directory");
        write(&m.folder(&x).join("record.json"), r);
    }
    let bypass = case == "parent-bound" || case == "team-bound";
    write(
        &m.folder(&x).join("spec.json"),
        json!({"flags": if bypass { json!(["--dangerously-skip-permissions"]) } else { json!([]) }}),
    );
    // prelife-1c G10d: the subject is a lifecycle member whose record proves it Ended (state-0 S6).
    crate::agents::prelife_fixture::to_lifecycle(&m.w, &x, true);
    if case == "parent-bound" {
        std::fs::remove_file(t::path(&m.state, "$3")).unwrap();
        team_record(&m.w, &["--dangerously-skip-permissions"]);
    }
    if case == "budget" {
        write(
            &crate::agents::budget::path(&m.state, "$3"),
            json!({"tokens": 0, "at": 0}),
        );
    }
    let mut rows: Vec<Value> = [LEADER, ID, OTHER].iter().map(|id| m.record(id)).collect();
    rows.extend(extra);
    if case == "limit" {
        // ID and OTHER and these fill the team besides the leader.
        for id in ids(MAX_MEMBERS - 2) {
            rows.push(cf::member(&m.w, &id, child(&id, LEADER, 1)));
        }
    }
    let (held, cand): (Held, bool) = (Default::default(), case == "candidate");
    if cand {
        // C is recorded only at the admission lock: the second `+spawn` (the first is the leaderless pre-check).
        let (lj, key, seen) = (
            m.folder(ID).join("launcher.json"),
            crate::agents::lock_key("spawn", &m.c, "$3"),
            std::cell::Cell::new(0),
        );
        write(&lj, json!({"pid": 31337, "start": 1000}));
        let (f, h, k2) = (m.w.fakes.clone(), held.clone(), key.clone());
        m.w.fakes.lock_probe(move |e| {
            if e == format!("+{key}") {
                seen.set(seen.get() + 1);
                if seen.get() == 2 {
                    write(
                        &lj,
                        json!({"pid": 31337, "start": 1000, "client": {"pid": 150, "start": 1500}}),
                    );
                }
            }
            String::new()
        });
        m.w.fakes.hc_probe(move |a| {
            if a[0] == "get" {
                let l = f.locks();
                let n = |s: &str| l.iter().filter(|e| **e == format!("{s}{k2}")).count();
                h.borrow_mut().push(n("+") > n("-"));
            }
        });
    }
    m.hc();
    m.rounds(if cand { 3 } else { 2 }, &term(&m.r, Some(150)));
    facts_of(&m.w, &crate::agents::membership::team(&m.c), &rows, &rx);
    if case == "ok" || case == "successor" || cand {
        launch_ok(&m.w);
    }
    let got = if cand {
        let (code, out) = crate::cli::run(&["resume".to_string(), x.clone()], || Ok(&m.w.ctx));
        let v: Value = serde_json::from_str(&out).unwrap();
        if code == 0 {
            Ok(v.as_object().unwrap().clone())
        } else {
            Err(crate::error::Refusal::internal(&out))
        }
    } else {
        crate::agents::resume::resume(&m.w.ctx, &m.w.request(&["resume", &x]))
    };
    (m, got, x, held)
}
// PM call 1 through admission: the exempt member caller still blocks X when its own argv names X's conversation, and
// a second unmanaged Claude whose own hook last reported X's conversation still blocks it (H4.4): the caller's
// exemption never covers another client. Each refusal names the pid and the rule (PM call 2).
#[test]
fn a_member_callers_exemption_ends_at_its_argv_and_other_clients() {
    use crate::agents::occupancy::Rule;
    for (case, pid, rule) in [
        ("caller-argv", 150, Rule::Argv),
        ("second-claude", 7001, Rule::Unmanaged),
    ] {
        let mut m = member_world();
        let x = "dddddddddddd".to_string();
        let mut rx = child(&x, ID, 2);
        let sid = "22222222-2222-4222-8222-222222222222";
        rx["sessionId"] = sid.into();
        cf::member(&m.w, &x, rx);
        write(&m.folder(&x).join("spec.json"), json!({"flags": []}));
        // prelife-1c G10d refuses a pre-lifecycle resume `state`; the subject is a lifecycle member, as restore-6b
        // moved its siblings, so occupancy's `session` check is the one under test.
        let rx = crate::agents::prelife_fixture::to_lifecycle(&m.w, &x, true);
        let rows: Vec<Value> = [LEADER, ID, OTHER].iter().map(|id| m.record(id)).collect();
        m.hc();
        if case == "caller-argv" {
            m.w.fakes.forget(150);
            m.w.fakes.process(Row {
                pid: 150,
                ppid: 31337,
                start: Some(1500),
                path: None,
                argv: Some(vec!["claude".into(), "--resume".into(), sid.into()]),
                image: None,
            });
        } else {
            let mut other = crate::testing::row(7001, 1, 7);
            other.argv = Some(vec!["claude".into()]);
            m.w.fakes.process(other);
            m.w.fakes.client(7001, crate::parse::Client::Claude);
            // Keyed as its hook writes it: its own (pid, start).
            let claude = crate::parse::Client::Claude;
            crate::state::live::report(&m.w.ctx, claude, 7001, 7, sid, "resume").unwrap();
        }
        m.rounds(2, &term(&m.r, Some(150)));
        facts_of(&m.w, &crate::agents::membership::team(&m.c), &rows, &rx);
        let e = crate::agents::resume::resume(&m.w.ctx, &m.w.request(&["resume", &x])).unwrap_err();
        assert_eq!(e.check, "session", "{case}: {}", e.message);
        assert!(
            e.message.starts_with(&format!("process {pid} ")),
            "{case}: {}",
            e.message
        );
        assert!(e.message.contains(rule.phrase()), "{case}: {}", e.message);
        m.w.fakes.hc_done().unwrap();
    }
}
#[test]
fn a_member_resumes_only_a_descendant() {
    let tokens = |m: &Mw| {
        crate::state::fs::read_json(&crate::agents::budget::path(&m.state, "$3"))
            .unwrap()
            .unwrap()["tokens"]
            .clone()
    };
    for case in ["ok", "candidate"] {
        let (m, got, x, held) = resume_case(case);
        let r = m.record(got.unwrap()["agent"]["id"].as_str().unwrap());
        assert_eq!(
            (&r["depth"], &r["parentAgent"]["id"], &r["resumedFrom"]),
            (&json!(2), &json!(ID), &json!(x)),
            "{case}: keeps depth"
        );
        assert_eq!(tokens(&m), 19, "{case}: a member spends");
        if case == "candidate" {
            assert_eq!(
                held.borrow()[..3],
                [false, false, true],
                "proven only while the admission lock is held"
            );
        }
        m.w.fakes.hc_done().unwrap();
    }
    for (case, check) in [
        ("non-descendant", "authority"),
        ("limit", "limit"),
        ("budget", "budget"),
        ("parent-bound", "ceiling"),
        ("team-bound", "ceiling"),
    ] {
        let (m, got, _, _) = resume_case(case);
        assert_eq!(got.unwrap_err().check, check, "{case}");
        assert!(
            !m.w.fakes
                .hc_calls()
                .iter()
                .any(|c| c.1.first().is_some_and(|a| a == "run")),
            "{case}: no launch"
        );
        if case == "budget" {
            assert_eq!(tokens(&m), 0, "an empty budget is unchanged");
        }
        m.w.fakes.hc_done().unwrap();
    }
}
/// A person's in-team resume spends nothing: it succeeds on an exhausted budget, which stays exhausted.
#[test]
fn a_persons_resume_is_free() {
    let w = World::human("");
    team_record(&w, &[]);
    let x = "dddddddddddd";
    let mut rx = child(x, LEADER, 1);
    rx["sessionId"] = json!("22222222-2222-4222-8222-222222222222");
    cf::member(&w, x, rx);
    write(&w.folder(x).join("spec.json"), json!({"flags": []}));
    let rx = crate::agents::prelife_fixture::to_lifecycle(&w, x, true); // prelife-1c G10d
    let (rows, budget) = (
        vec![cf::member(&w, LEADER, leader_record())],
        crate::agents::budget::path(&w.state(), "$3"),
    );
    write(&budget, json!({"tokens": 0, "at": 0}));
    cf::online(&w);
    facts_of(
        &w,
        &crate::agents::membership::team(&w.caller()),
        &rows,
        &rx,
    );
    launch_ok(&w);
    crate::agents::resume::resume(&w.ctx, &w.request(&["resume", x])).unwrap();
    assert_eq!(
        crate::state::fs::read_json(&budget).unwrap().unwrap()["tokens"],
        0
    );
    w.fakes.hc_done().unwrap();
}
/// A person's resume where limit, ceiling and name all fail: a lifecycle subject replays its stored options against
/// the team ceiling first (E2, p1c delivery call: fail closed), then limit, then name.
#[test]
fn resume_refuses_in_v5_order() {
    // The twin and `more` children are the members besides the leader: `MAX_MEMBERS - 1` more fills the team.
    for (more, bypass, check) in [
        (MAX_MEMBERS - 1, false, "ceiling"),
        (MAX_MEMBERS - 1, true, "limit"),
        (MAX_MEMBERS - 2, false, "ceiling"),
        (MAX_MEMBERS - 2, true, "name"),
    ] {
        let w = World::human("");
        team_record(
            &w,
            if bypass {
                &["--dangerously-skip-permissions"][..]
            } else {
                &[]
            },
        );
        let (x, twin) = ("dddddddddddd", "eeeeeeeeeeee");
        let mut rx = child(x, LEADER, 1);
        (rx["name"], rx["sessionId"]) = (
            json!("worker"),
            json!("22222222-2222-4222-8222-222222222222"),
        );
        cf::member(&w, x, rx);
        write(
            &w.folder(x).join("spec.json"),
            json!({"flags": ["--dangerously-skip-permissions"]}),
        );
        let rx = crate::agents::prelife_fixture::to_lifecycle(&w, x, true); // prelife-1c G10d
        let mut rt = child(twin, LEADER, 1);
        rt["name"] = json!("worker");
        let mut rows = vec![
            cf::member(&w, LEADER, leader_record()),
            cf::member(&w, twin, rt),
        ];
        for id in ids(more) {
            rows.push(cf::member(&w, &id, child(&id, LEADER, 1)));
        }
        cf::online(&w);
        facts_of(
            &w,
            &crate::agents::membership::team(&w.caller()),
            &rows,
            &rx,
        );
        let e = crate::agents::resume::resume(&w.ctx, &w.request(&["resume", x])).unwrap_err();
        assert_eq!(e.check, check, "{more} {bypass}");
        w.fakes.hc_done().unwrap();
    }
    let w = World::human("");
    w.uncommitted(LEADER, json!({"depth": 0, "upTeam": "t"}));
    assert_eq!(
        crate::agents::resume::resume(&w.ctx, &w.request(&["resume", "worker"]))
            .unwrap_err()
            .check,
        "team"
    );
    assert!(
        w.fakes.hc_calls().is_empty(),
        "leaderless refuses before hc"
    );
}
/// Through the CLI: a pre-lifecycle candidate is refused 2 `state` from its record (G10d, before `tagged` and any
/// hc call; p1c delivery call) and makes no folder.
#[test]
fn a_candidate_resume_is_refused_with_no_side_effects() {
    let m = member_world();
    let before = std::fs::read_dir(m.state.join("agents")).unwrap().count();
    let (code, out) = crate::cli::run(&["resume", "mbb"].map(String::from), || Ok(&m.w.ctx));
    assert_eq!(
        (
            code,
            out.contains(r#""check":"state""#),
            out.contains("pre-lifecycle")
        ),
        (2, true, true),
        "{out}"
    );
    assert!(m.w.fakes.hc_calls().is_empty(), "refused before hc");
    assert_eq!(
        std::fs::read_dir(m.state.join("agents")).unwrap().count(),
        before
    );
    m.w.fakes.hc_done().unwrap();
}
/// A request refused by itself spends no token (D5): a proven member's brief child with no -BriefFile refuses 2
/// `brief` under the team lock, after its standing and the snapshot, before the debit and any launch.
#[test]
fn a_refused_request_spends_no_token() {
    let mut m = member_world();
    m.w.file("t.md", b"task");
    m.hc();
    let t = term(&m.r, Some(150));
    m.rounds(2, &t);
    // What the launch would read under the team lock: connect, the caller's own terminal, then the snapshot.
    cf::online(&m.w);
    cf::get(&m.w, m.r["hcAddress"].as_str().unwrap(), t.clone());
    cf::listing(&m.w, &crate::agents::membership::team(&m.c), &[]);
    let mut all: Vec<(String, Value)> = [LEADER, ID, OTHER]
        .iter()
        .map(|id| {
            let r = m.record(id);
            (
                r["hcAddress"].as_str().unwrap().to_string(),
                if *id == ID { t.clone() } else { cf::wire(&r) },
            )
        })
        .collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    for (address, wire) in all {
        cf::get(&m.w, &address, wire);
    }
    let args = [
        "spawn",
        "-Client",
        "claude",
        "-TaskFile",
        "t.md",
        "-Context",
        "brief",
        "-Name",
        "child",
    ]
    .map(String::from);
    let (code, out) = crate::cli::run(&args, || Ok(&m.w.ctx));
    assert_eq!(
        (code, out.contains(r#""check":"brief""#)),
        (2, true),
        "{out}"
    );
    assert!(
        !crate::agents::budget::path(&m.state, "$3").exists(),
        "no token spent"
    );
    assert!(
        !m.w.fakes
            .hc_calls()
            .iter()
            .any(|c| c.1.first().is_some_and(|a| a == "run")),
        "refused before any launch"
    );
}
/// What a proven member's spawn reads from hc under the team lock: its two resolutions, then the launch's connect,
/// its own terminal and the snapshot of LEADER, ID and OTHER.
fn spawn_rounds(m: &Mw, t: &Value) {
    m.rounds(2, t);
    cf::online(&m.w);
    cf::get(&m.w, m.r["hcAddress"].as_str().unwrap(), t.clone());
    cf::listing(&m.w, &crate::agents::membership::team(&m.c), &[]);
    let mut all: Vec<(String, Value)> = [LEADER, ID, OTHER]
        .iter()
        .map(|id| {
            let r = m.record(id);
            (
                r["hcAddress"].as_str().unwrap().to_string(),
                if *id == ID { t.clone() } else { cf::wire(&r) },
            )
        })
        .collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    for (address, wire) in all {
        cf::get(&m.w, &address, wire);
    }
}
/// A request check that only the launch's own preparation can make (the escaped command line) spends no token: the
/// debit is the last step before `hc run` (D5).
#[test]
fn a_command_line_too_long_spends_no_token() {
    let mut m = member_world();
    m.w.file("t.md", &vec![b'"'; crate::state::fs::TASK_MAX as usize]);
    m.w.file("b.md", b"brief");
    m.hc();
    spawn_rounds(&m, &term(&m.r, Some(150)));
    let args = [
        "spawn",
        "-Client",
        "claude",
        "-TaskFile",
        "t.md",
        "-BriefFile",
        "b.md",
        "-Context",
        "brief",
        "-Name",
        "child",
    ]
    .map(String::from);
    let (code, out) = crate::cli::run(&args, || Ok(&m.w.ctx));
    assert_eq!(
        (
            code,
            out.contains(r#""check":"usage""#),
            out.contains("too long")
        ),
        (2, true, true),
        "{out}"
    );
    assert!(
        !crate::agents::budget::path(&m.state, "$3").exists(),
        "no token spent"
    );
    assert!(
        !m.w.fakes
            .hc_calls()
            .iter()
            .any(|c| c.1.first().is_some_and(|a| a == "run")),
        "no launch"
    );
}
/// A refusal from inside `allocate` spends no token: a Copilot member's fork whose session copy cannot be verified
/// turns brief, and with no -BriefFile it refuses 2 `brief` before the debit.
#[test]
fn a_refusal_inside_allocate_spends_no_token() {
    let mut m = member_world();
    m.w.file("t.md", b"task");
    let mut r = m.record(ID);
    r["client"] = "copilot".into();
    write(&m.folder(ID).join("record.json"), r);
    m.w.fakes.client(150, Client::Copilot);
    m.hc();
    m.w.ctx.env = crate::ctx::Env::from_pairs([
        ("HC_TERMINAL_ID", m.r["hcAddress"].as_str().unwrap()),
        (
            "COPILOT_AGENT_SESSION_ID",
            "33333333-3333-4333-8333-333333333333",
        ),
    ]);
    // No session folder to copy: `copy_session` fails before it writes anything.
    m.w.ctx.home = m.w.ctx.cwd.join("no-home");
    spawn_rounds(&m, &term(&m.r, Some(150)));
    let args = [
        "spawn",
        "-Client",
        "copilot",
        "-TaskFile",
        "t.md",
        "-Context",
        "fork",
        "-Name",
        "child",
    ]
    .map(String::from);
    let (code, out) = crate::cli::run(&args, || Ok(&m.w.ctx));
    assert_eq!(
        (code, out.contains(r#""check":"brief""#)),
        (2, true),
        "{out}"
    );
    assert!(
        !crate::agents::budget::path(&m.state, "$3").exists(),
        "no token spent"
    );
    assert!(
        !m.w.fakes
            .hc_calls()
            .iter()
            .any(|c| c.1.first().is_some_and(|a| a == "run")),
        "no launch"
    );
}
/// A guard: a member's spawn that launches spends exactly one token.
#[test]
fn a_member_spawn_that_launches_spends_one_token() {
    let mut m = member_world();
    m.w.file("t.md", b"task");
    m.w.file("b.md", b"brief");
    m.hc();
    spawn_rounds(&m, &term(&m.r, Some(150)));
    launch_ok(&m.w);
    let args = [
        "spawn",
        "-Client",
        "claude",
        "-TaskFile",
        "t.md",
        "-BriefFile",
        "b.md",
        "-Context",
        "brief",
        "-Name",
        "child",
    ]
    .map(String::from);
    let (code, out) = crate::cli::run(&args, || Ok(&m.w.ctx));
    assert_eq!(code, 0, "{out}");
    let budget = crate::state::fs::read_json(&crate::agents::budget::path(&m.state, "$3"))
        .unwrap()
        .unwrap();
    assert_eq!(budget["tokens"], 19, "one token, at the launch");
    m.w.fakes.hc_done().unwrap();
}
/// A guard: a failure that cannot be known before the launch keeps its token. `hc run` fails after the debit (D5).
#[test]
fn a_failed_launch_keeps_its_token() {
    let mut m = member_world();
    m.w.file("t.md", b"task");
    m.w.file("b.md", b"brief");
    m.hc();
    spawn_rounds(&m, &term(&m.r, Some(150)));
    m.w.fakes
        .hc_answer(Err(crate::error::Refusal::internal("timeout")));
    let args = [
        "spawn",
        "-Client",
        "claude",
        "-TaskFile",
        "t.md",
        "-BriefFile",
        "b.md",
        "-Context",
        "brief",
        "-Name",
        "child",
    ]
    .map(String::from);
    let (code, out) = crate::cli::run(&args, || Ok(&m.w.ctx));
    assert_ne!(code, 0, "{out}");
    let budget = crate::state::fs::read_json(&crate::agents::budget::path(&m.state, "$3"))
        .unwrap()
        .unwrap();
    assert_eq!(budget["tokens"], 19, "the run failed after the debit");
    assert!(
        m.w.fakes
            .hc_calls()
            .iter()
            .any(|c| c.1.first().is_some_and(|a| a == "run")),
        "the run was tried"
    );
}
/// The run's own inputs are checked before the token (D5): a member's resume of a descendant whose record has no
/// `directory` refuses with no token spent, no `hc run`, and no abort that says a run may have happened.
#[test]
fn a_run_input_that_cannot_be_built_spends_no_token() {
    let (m, got, x, _) = resume_case("no-directory");
    assert!(got.is_err(), "{got:?}");
    assert!(
        !crate::agents::budget::path(&m.state, "$3").exists(),
        "no token spent: {got:?}"
    );
    assert!(
        !m.w.fakes
            .hc_calls()
            .iter()
            .any(|c| c.1.first().is_some_and(|a| a == "run")),
        "no launch"
    );
    for entry in std::fs::read_dir(m.state.join("agents")).unwrap() {
        let folder = entry.unwrap().path();
        let abort = folder.join("abort.json");
        if folder.ends_with(&x) || !abort.is_file() {
            continue;
        }
        let a = crate::state::fs::read_json(&abort).unwrap().unwrap();
        assert_ne!(a["runUncertain"], true, "nothing ran: {a}");
    }
}
/// Under the team lock the proof is whole again (D1): a terminal that no longer runs the recorded launch, or a
/// launch aborted since dispatch, refuses 2 `tagged`, never acting on what dispatch saw.
#[test]
fn standing_revalidates_the_launch_under_the_lock() {
    for case in ["argv", "aborted"] {
        let mut m = member_world();
        m.hc();
        let t = term(&m.r, Some(150));
        m.rounds(1, &t);
        let c = crate::caller::resolve_tree(&m.w.ctx).unwrap();
        assert_eq!(c.member.as_deref(), Some(ID), "{case}: proven at dispatch");
        let mut later = t.clone();
        match case {
            "argv" => later["argv"] = json!(["/bin/sh", "-l"]),
            _ => write(&m.folder(ID).join("commit.json"), json!({"aborted": true})),
        }
        m.rounds(1, &later);
        assert_eq!(standing(&m.w.ctx, &c, None), Err(tagged()), "{case}");
        m.w.fakes.hc_done().unwrap();
    }
}
/// The same through a spawn: the terminal changes between dispatch and the team lock; the spawn refuses 2 `tagged`
/// before any reconcile, debit or launch.
#[test]
fn a_spawn_whose_terminal_changes_before_the_lock_refuses() {
    let mut m = member_world();
    m.w.file("t.md", b"task");
    m.w.file("b.md", b"brief");
    m.hc();
    let t = term(&m.r, Some(150));
    m.rounds(2, &t);
    let mut later = t.clone();
    later["argv"] = json!(["/bin/sh", "-l"]);
    m.rounds(1, &later);
    let args = [
        "spawn",
        "-Client",
        "claude",
        "-TaskFile",
        "t.md",
        "-BriefFile",
        "b.md",
        "-Context",
        "brief",
    ]
    .map(String::from);
    let (code, out) = crate::cli::run(&args, || Ok(&m.w.ctx));
    assert_eq!(
        (code, out.contains(r#""check":"tagged""#)),
        (2, true),
        "{out}"
    );
    assert!(
        !crate::agents::budget::path(&m.state, "$3").exists(),
        "no token spent"
    );
    assert!(
        !m.w.fakes
            .hc_calls()
            .iter()
            .any(|c| c.1.first().is_some_and(|a| a == "ls" || a == "run")),
        "no reconcile, no launch"
    );
    m.w.fakes.hc_done().unwrap();
}
/// Ancestry ends only at the team's own leader: a second depth-0 `upTeam` record R in the session, with
/// R > A > B, gives A no team standing, so its spawn admission and its stop and read of B refuse 2 `authority`.
#[test]
fn a_second_root_grants_no_authority() {
    let w = World::human("");
    let c = w.caller();
    team_record(&w, &[]);
    cf::member(&w, LEADER, leader_record());
    let rogue = "cccccccccccc";
    cf::member(
        &w,
        rogue,
        json!({"id": rogue, "name": "rogue", "client": "claude", "session": "$3", "depth": 0,
        "upTeam": "t", "parentAgent": null, "pane": "%5"}),
    );
    let i = ids(2);
    cf::member(&w, &i[0], child(&i[0], rogue, 1));
    cf::member(&w, &i[1], child(&i[1], &i[0], 2));
    let rec = |id: &str| {
        crate::state::fs::read_json(&w.folder(id).join("record.json"))
            .unwrap()
            .unwrap()
    };
    let team = team_of(&w.ctx, &c).unwrap().unwrap();
    assert_eq!(team.leader, LEADER);
    let e = admit(&w.ctx, &c, &team, member_of(&i[0], rec(&i[0]))).unwrap_err();
    assert_eq!(
        (e.check, e.message.as_str()),
        ("authority", "the parent chain of ma0 cannot be read"),
        "spawn admission"
    );
    for verb in ["stop", "read"] {
        assert_eq!(
            authority(&w.ctx, &c, &member_of(&i[0], rec(&i[0])), &i[1], verb),
            refuse(2, "authority", "the parent chain of ma1 cannot be read"),
            "{verb}"
        );
    }
}
/// A resume reads launch specs only inside the state root: a parent's spec.json linked to a file outside
/// refuses, never resuming with flags read through the link. A target whose own spec.json is linked is a
/// pre-lifecycle folder (the folder spec is read only on that path), so G10d refuses it `state` before any read of
/// the link (prelife-1c Task 4: retired as written), and the link target is untouched.
#[test]
fn a_linked_spec_refuses_a_resume() {
    for case in ["target", "parent"] {
        let w = World::human("");
        team_record(&w, &[]);
        let (x, p) = ("dddddddddddd", "eeeeeeeeeeee");
        let mut rows = vec![cf::member(&w, LEADER, leader_record())];
        let mut rx = child(
            x,
            if case == "parent" { p } else { LEADER },
            if case == "parent" { 2 } else { 1 },
        );
        if case == "parent" {
            rows.push(cf::member(&w, p, child(p, LEADER, 1)));
            write(&w.folder(p).join("spec.json"), json!({"flags": []}));
        }
        rx["sessionId"] = json!("22222222-2222-4222-8222-222222222222");
        let mut rx = cf::member(&w, x, rx);
        write(&w.folder(x).join("spec.json"), json!({"flags": []}));
        if case == "parent" {
            rx = crate::agents::prelife_fixture::to_lifecycle(&w, x, true); // prelife-1c G10d
        }
        let outside = w._dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let spec = w
            .folder(if case == "parent" { p } else { x })
            .join("spec.json");
        let moved = outside.join("spec.json");
        std::fs::rename(&spec, &moved).unwrap();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&moved, &spec);
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&moved, &spec);
        if let Err(e) = linked {
            eprintln!("{case}: skipped, no link: {e}");
            continue;
        }
        let outside_bytes = std::fs::read(&moved).unwrap();
        if case == "parent" {
            cf::online(&w);
            facts_of(
                &w,
                &crate::agents::membership::team(&w.caller()),
                &rows,
                &rx,
            );
        }
        let e = crate::agents::resume::resume(&w.ctx, &w.request(&["resume", x])).unwrap_err();
        let want = match case {
            "target" => (
                "state",
                "mdd predates lifecycle records (pre-lifecycle) and is never resumed",
            ),
            _ => ("authority", "the parent chain of mdd cannot be read"),
        };
        assert_eq!((e.check, e.message.as_str()), want, "{case}");
        assert_eq!(
            std::fs::read(&moved).unwrap(),
            outside_bytes,
            "{case}: link target untouched"
        );
        assert!(
            !w.fakes
                .hc_calls()
                .iter()
                .any(|c| c.1.first().is_some_and(|a| a == "run")),
            "{case}: no launch"
        );
        w.fakes.hc_done().unwrap();
    }
}
/// A person may still stop or read a committed leader whose team record `up` never wrote: authority asks a
/// person for no team record; the target's identity and view are still checked.
#[test]
fn a_person_stops_and_reads_a_leaderless_teams_leader() {
    for verb in ["read", "stop"] {
        let w = World::human("");
        let r = cf::member(&w, LEADER, leader_record());
        let mut row = crate::agents::fixture::row_of("%4|@5|300||", &[]);
        row.command = r["startCommand"].as_str().unwrap().into();
        w.pane_rows(&[row]);
        w.fakes.mux_rule("show-buffer", 0, "one\ntwo\n", "");
        let a = r["hcAddress"].as_str().unwrap().to_string();
        if verb == "stop" {
            cf::online(&w);
            // prelife-1c P-R3: the validating get (Running) is the last hc call before the kill.
            cf::get(&w, &a, cf::wire(&r));
            cf::answer(&w, &["kill", &a, "--json"], "{}");
            cf::get(&w, &a, cf::wire(&r));
        }
        let (code, out) = crate::cli::run(&[verb, "leader"].map(String::from), || Ok(&w.ctx));
        assert_eq!(code, 0, "{verb}: {out}");
        w.fakes.hc_done().unwrap();
    }
    let w = World::human("");
    cf::member(&w, LEADER, leader_record());
    let e = crate::agents::spawn::spawn(
        &w.ctx,
        &w.request(&["spawn", "-Client", "claude", "-Task", "hi"]),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.check),
        (1, "team"),
        "a spawn still refuses without the team record"
    );
}

#[test]
fn the_hook_chain_proves_and_a_non_shell_link_is_tagged() {
    // The hook runs mux-agent via sh under the client: caller, shell, client, launcher is the D1 chain.
    let mut m = member_world();
    m.hc();
    m.rounds(1, &term(&m.r, Some(150)));
    assert_eq!(
        crate::caller::hc::resolve(&m.w.ctx)
            .unwrap()
            .member
            .as_deref(),
        Some(ID),
        "the hook chain proves"
    );
    // A non-shell host process between the shell (170) and the client (150).
    m.w.fakes.process(crate::testing::row(171, 150, 1710));
    m.w.fakes.forget(170);
    m.w.fakes.process(crate::testing::row(170, 171, 1700));
    m.w.fakes.shell(170);
    m.rounds(1, &term(&m.r, Some(150)));
    let got = crate::caller::hc::resolve(&m.w.ctx).unwrap();
    assert_eq!(
        (got.member, got.candidate.as_deref()),
        (None, Some(ID)),
        "a candidate"
    );
    m.rounds(1, &term(&m.r, Some(150)));
    assert_eq!(
        crate::caller::resolve(&m.w.ctx).unwrap_err().check,
        "tagged",
        "every verb refuses it, read -Count included"
    );
}

// A2a with 4b-ii: a lifecycle member with stored options reports done over its D1 hook chain while an owner-matched
// `read -Wait` lease is live: the caller is proven, done is recorded, and the lease stays.
#[test]
fn a_lifecycle_member_reports_done_over_its_hook_chain_and_keeps_its_lease() {
    let mut m = member_world();
    let mut r = m.record(ID);
    (r["lifecycle"], r["options"]) = (json!(1), json!(["--model", "x"]));
    write(&m.folder(ID).join("record.json"), r);
    // K2: a lifecycle member is proven from its active committed generation's evidence.
    evidence(&m, 0);
    commit(&m, 0, json!({"committed": true}));
    m.w.fakes.process(crate::testing::row(77, 1, 770));
    let (f, x, name) = (m.folder(ID), m.record(ID), format!("m{}", &ID[..8]));
    crate::state::evidence::publish(
        &crate::agents::gens::path(&f, 0, "launch"),
        &json!({"hcName": name}),
    )
    .unwrap();
    write(
        &f.join("view.json"),
        json!({"pane": x["pane"], "window": x["window"], "startCommand": x["startCommand"], "gen": 0, "hcAddress": x["hcAddress"], "hcName": name}),
    );
    let lease = m.folder(ID).join("waiting-77.json");
    write(
        &lease,
        json!({"pid": 77, "start": 770, "since": "2026-10-29T12:00:00.0000000Z",
            "owner": {"id": ID, "gen": "0"}, "until": "2999-01-01T00:00:00.0000000Z"}),
    );
    m.hc();
    let t = term(&m.r, Some(150));
    // The explicit resolution, report's own, then report's snapshot of LEADER, ID and OTHER.
    m.rounds(2, &t);
    cf::online(&m.w);
    cf::listing(&m.w, &crate::agents::membership::team(&m.c), &[]);
    let mut all: Vec<(String, Value)> = [LEADER, ID, OTHER]
        .iter()
        .map(|id| {
            let r = m.record(id);
            let wire = if *id == ID { t.clone() } else { cf::wire(&r) };
            (r["hcAddress"].as_str().unwrap().to_string(), wire)
        })
        .collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    for (address, wire) in all {
        cf::get(&m.w, &address, wire);
    }
    assert_eq!(
        crate::caller::hc::resolve(&m.w.ctx)
            .unwrap()
            .member
            .as_deref(),
        Some(ID),
        "the hook chain proves the lifecycle member"
    );
    let got = crate::agents::view::report(&m.w.ctx, &m.w.request(&["report", "done"]));
    assert_eq!(got.map(|r| r["state"].clone()), Ok(json!("done")));
    let activity = crate::state::fs::read_json(&m.folder(ID).join("activity.json"));
    assert_eq!(activity.unwrap().unwrap()["state"], "done");
    assert!(lease.exists(), "the live lease of the current owner stays");
    m.w.fakes.hc_done().unwrap();
}

// E2: the level a lifecycle parent grants is read from its options, not from a folder-level spec.
#[test]
fn a_lifecycle_parents_level_is_read_from_its_options() {
    let m = member_world();
    let found = t::read(&t::path(&m.state, "$3")).unwrap().unwrap();
    let grandchild = "cccccccccccc";
    let mut parent = m.record(ID);
    parent["lifecycle"] = json!(1);
    parent["options"] = json!(t::permission_flags(Client::Claude, t::Policy::Bypass));
    let mut records = BTreeMap::new();
    records.insert(ID.to_string(), parent.clone());
    records.insert(grandchild.to_string(), child(grandchild, ID, 2));
    let level = |records: &BTreeMap<String, Value>| {
        parent_level(&m.w.ctx, &m.c, &found, records, grandchild)
    };
    assert_eq!(level(&records), Ok(t::Policy::Bypass));
    parent.as_object_mut().unwrap().remove("lifecycle");
    records.insert(ID.to_string(), parent);
    assert_eq!(
        level(&records),
        Ok(t::Policy::Default),
        "pre-lifecycle: the spec, --effort high"
    );
}

/// A person's in-team resume of lifecycle member X (the leader's child) whose stored options are `options`, in a team
/// `up` made with `up_flags`. X's folder-level spec says `--effort high`, so only the options can give it Bypass.
/// Returns the world, the result, and the server folder's tree just before the resume.
fn lifecycle_resume(
    up_flags: &[String],
    options: &[String],
    launches: bool,
) -> (World, Res<Map<String, Value>>, crate::testing::Tree) {
    let w = World::human("");
    team_record(&w, &up_flags.iter().map(String::as_str).collect::<Vec<_>>());
    let x = "dddddddddddd";
    let mut rx = child(x, LEADER, 1);
    (rx["sessionId"], rx["lifecycle"], rx["options"]) = (
        json!("22222222-2222-4222-8222-222222222222"),
        json!(1),
        json!(options),
    );
    let rx = cf::member(&w, x, rx);
    // R3: hc's exited alone no longer ends a lifecycle member; its generation 0 is committed and its end recorded.
    for (file, v) in [
        ("commit", json!({"committed": true})),
        ("ended", json!({"state": "finished", "reason": "exited"})),
    ] {
        crate::state::evidence::publish(&crate::agents::gens::path(&w.folder(x), 0, file), &v)
            .unwrap();
    }
    write(
        &w.folder(x).join("spec.json"),
        json!({"flags": ["--effort", "high"]}),
    );
    let rows = vec![cf::member(&w, LEADER, leader_record())];
    cf::online(&w);
    facts_of(
        &w,
        &crate::agents::membership::team(&w.caller()),
        &rows,
        &rx,
    );
    if launches {
        launch_ok(&w);
    }
    let before = crate::testing::tree(&w.state());
    let got = crate::agents::resume::resume(&w.ctx, &w.request(&["resume", x]));
    (w, got, before)
}

// E2 in a team: bypass options stored under a Bypass team replay as they are.
#[test]
fn a_lifecycle_member_replays_bypass_options_under_a_bypass_team() {
    let bypass = t::permission_flags(Client::Claude, t::Policy::Bypass);
    let (w, got, _) = lifecycle_resume(&bypass, &bypass, true);
    let y = got.unwrap()["agent"]["id"].as_str().unwrap().to_string();
    let spec = crate::state::fs::read_json(&crate::agents::gens::path(&w.folder(&y), 0, "spec"))
        .unwrap()
        .unwrap();
    assert_eq!(
        spec["flags"],
        json!(bypass),
        "replayed within the Bypass ceiling"
    );
    w.fakes.hc_done().unwrap();
}

// E2 in a team: options above the team ceiling are refused by replay itself (its message, not `bound`'s), and
// nothing is launched or made.
#[test]
fn a_lifecycle_member_above_the_team_ceiling_is_refused() {
    let bypass = t::permission_flags(Client::Claude, t::Policy::Bypass);
    let (w, got, before) = lifecycle_resume(&[], &bypass, false);
    let e = got.unwrap_err();
    assert_eq!(
        (e.code, e.check, e.message.as_str()),
        (2, "ceiling", "the replayed options exceed the team ceiling")
    );
    assert!(
        !w.fakes.hc_calls().iter().any(|c| c.1[0] == "run"),
        "nothing launched"
    );
    assert_eq!(crate::testing::tree(&w.state()), before, "nothing made");
    w.fakes.hc_done().unwrap();
}

// proof-0b K2: a lifecycle member is proven from its committed generation's client.json alone; neither folder-level
// launcher.json nor spec.json is read. Without client.json it is unproven.
#[test]
fn a_lifecycle_member_is_proven_from_its_committed_generations_client_json() {
    let m = lifecycle_world(0);
    std::fs::remove_file(m.folder(ID).join("spec.json")).unwrap();
    evidence(&m, 0);
    commit(&m, 0, json!({"committed": true}));
    let x = prove(&m, ID, Some(150)).expect("proven from client.json alone");
    assert_eq!(
        (x.pid, x.start, x.flags.as_slice()),
        (150, 1500, &["--model".to_string(), "x".into()][..])
    );
    std::fs::remove_file(crate::agents::gens::path(&m.folder(ID), 0, "client")).unwrap();
    assert_eq!(prove(&m, ID, Some(150)), None, "no client.json: unproven");
    m.w.fakes.hc_done().unwrap();
}

// K2: only the active generation proves, and only its client. Generation 0 is whole, committed and names this very
// client, so a proof that read any of it would succeed. The record names generation 1, whose files are all present
// (so each reads Present): aborted, it proves nothing; committed, it proves; with a client.json naming another
// process, nothing again.
#[test]
fn only_the_active_generation_proves_a_lifecycle_member() {
    let m = lifecycle_world(1);
    evidence(&m, 0);
    commit(&m, 0, json!({"committed": true}));
    evidence(&m, 1);
    commit(&m, 1, json!({"aborted": true}));
    assert_eq!(
        prove(&m, ID, Some(150)),
        None,
        "generation 0's commit never decides generation 1"
    );
    std::fs::write(
        crate::agents::gens::path(&m.folder(ID), 1, "commit"),
        br#"{"committed":true}"#,
    )
    .unwrap();
    assert!(
        matches!(prove(&m, ID, Some(150)), Some(x) if x.pid == 150),
        "generation 1 committed"
    );
    std::fs::write(
        crate::agents::gens::path(&m.folder(ID), 1, "client"),
        br#"{"pid":152,"start":1500}"#,
    )
    .unwrap();
    assert_eq!(
        prove(&m, ID, Some(150)),
        None,
        "generation 0's client never stands in"
    );
    m.w.fakes.hc_done().unwrap();
}

// K2 through the lifecycle branch: only a commit that decides Run (gens::decide) proves. Absent, aborted, invalid,
// contradictory and unreadable each leave the member unproven, never refused. The first case is the control.
#[test]
fn a_lifecycle_member_is_proven_only_by_a_committed_decision() {
    for (case, body) in [
        ("committed", Some(&br#"{"committed":true}"#[..])),
        ("absent", None),
        ("aborted", Some(&br#"{"aborted":true}"#[..])),
        ("invalid", Some(&br#"{"committed":"true"}"#[..])),
        (
            "contradictory",
            Some(&br#"{"committed":true,"aborted":true}"#[..]),
        ),
        ("unreadable", Some(&br#"{"comm"#[..])),
    ] {
        let m = lifecycle_world(0);
        evidence(&m, 0);
        if let Some(bytes) = body {
            std::fs::write(crate::agents::gens::path(&m.folder(ID), 0, "commit"), bytes).unwrap();
        }
        assert_eq!(
            prove(&m, ID, Some(150)).is_some(),
            case == "committed",
            "{case}"
        );
        m.w.fakes.hc_done().unwrap();
    }
}

// K1a's veto through K2: a committed generation whose exit.json says client-conflict, or is torn, proves nothing,
// though its launcher.json and client.json name the live pair. A clean exit.json is the control.
#[test]
fn a_lifecycle_member_whose_exit_vetoes_its_client_is_unproven() {
    for (case, bytes) in [
        ("clean exit", &br#"{"exitCode":0}"#[..]),
        (
            "client-conflict",
            &br#"{"exitCode":1,"reason":"client-conflict"}"#[..],
        ),
        ("torn exit", &br#"{"exitC"#[..]),
    ] {
        let m = lifecycle_world(0);
        evidence(&m, 0);
        commit(&m, 0, json!({"committed": true}));
        std::fs::write(crate::agents::gens::path(&m.folder(ID), 0, "exit"), bytes).unwrap();
        assert_eq!(
            prove(&m, ID, Some(150)).is_some(),
            case == "clean exit",
            "{case}"
        );
        m.w.fakes.hc_done().unwrap();
    }
}

// K1a/K2 for Copilot on generation evidence: client.json names the loader 150 (the launcher's child); the caller's
// client is its native 155. A native 156 under another loader 160 of the same launcher (start 1500, like 150) is
// unrelated: only the recorded loader may pair. Unix only, as `copilot_pair` is.
#[cfg(unix)]
#[test]
fn a_lifecycle_copilot_member_is_proven_by_its_pair_only() {
    for case in ["the pair", "unrelated native"] {
        let m = lifecycle_world(0);
        let mut r = m.record(ID);
        r["client"] = json!("copilot");
        write(&m.folder(ID).join("record.json"), r);
        evidence(&m, 0);
        commit(&m, 0, json!({"committed": true}));
        m.w.fakes.forget(150);
        m.w.fakes.process(row(
            150,
            31337,
            1500,
            "/usr/bin/node",
            &["node", "/usr/local/bin/copilot"],
        ));
        m.w.fakes
            .process(row(155, 150, 1550, "/opt/copilot/copilot", &["copilot"]));
        let native = if case == "the pair" { 155 } else { 156 };
        if case == "unrelated native" {
            m.w.fakes.process(row(
                160,
                31337,
                1500,
                "/usr/bin/node",
                &["node", "/usr/local/bin/copilot"],
            ));
            m.w.fakes
                .process(row(156, 160, 1560, "/opt/copilot/copilot", &["copilot"]));
        }
        for pid in [150, 155, 156, 160] {
            m.w.fakes.client(pid, Client::Copilot);
        }
        m.w.fakes.forget(170);
        m.w.fakes.process(crate::testing::row(170, native, 1700));
        let got = prove(&m, ID, Some(native as i32));
        if case == "the pair" {
            assert!(
                matches!(got, Some(x) if x.pid == 150 && x.client == Client::Copilot),
                "{case}"
            );
        } else {
            assert_eq!(got, None, "{case}");
        }
        m.w.fakes.hc_done().unwrap();
    }
}

// E2: a proven lifecycle member's flags are its stored options, never its spec (`--effort high`); none: unproven.
// (A2a Task 1's test, moved onto committed generation-0 evidence; both assertions kept.)
#[test]
fn a_proven_lifecycle_members_flags_are_its_stored_options() {
    let m = lifecycle_world(0);
    evidence(&m, 0);
    commit(&m, 0, json!({"committed": true}));
    assert_eq!(
        prove(&m, ID, Some(150)).map(|x| x.flags),
        Some(vec!["--model".to_string(), "x".into()])
    );
    let mut r = m.record(ID);
    r.as_object_mut().unwrap().remove("options");
    write(&m.folder(ID).join("record.json"), r);
    assert_eq!(
        prove(&m, ID, Some(150)),
        None,
        "no stored options: unproven"
    );
    m.w.fakes.hc_done().unwrap();
}

// K2, point 8: the shared reader opens only regular files reached through real folders. A file or folder link out
// of the member folder, or a dangling exit.json, proves nothing, and the linked name is never opened. Every OS; the
// record is passed in, because `prove` would read record.json through its link.
#[test]
fn lifecycle_evidence_reached_through_a_link_proves_nothing() {
    use crate::agents::gens::{dir, path};
    const GEN: [&str; 4] = ["client.json", "commit.json", "exit.json", "launcher.json"];
    for case in [
        "record",
        "commit",
        "launcher",
        "client",
        "exit",
        "dangling exit",
        "gens/0",
    ] {
        let m = lifecycle_world(0);
        evidence(&m, 0);
        commit(&m, 0, json!({"committed": true}));
        let f = m.folder(ID);
        crate::state::evidence::publish(&path(&f, 0, "exit"), &json!({"exitCode": 0})).unwrap();
        let r = m.record(ID);
        let (got, reads) = prove_reads(&m, ID, &r);
        assert!(got.is_some(), "{case}: proven before the link");
        let mut seen: Vec<_> = reads
            .iter()
            .filter_map(|p| p.file_name()?.to_str().map(String::from))
            .collect();
        seen.sort();
        assert_eq!(
            seen,
            [
                "client.json",
                "commit.json",
                "exit.json",
                "launcher.json",
                "record.json"
            ],
            "{case}: the seam"
        );
        let (at, never): (PathBuf, Vec<String>) = match case {
            "record" => (f.join("record.json"), vec!["record.json".into()]),
            "gens/0" => (dir(&f, 0), GEN.iter().map(|n| n.to_string()).collect()),
            "dangling exit" => (path(&f, 0, "exit"), vec!["exit.json".into()]),
            file => (path(&f, 0, file), vec![format!("{file}.json")]),
        };
        let outside = crate::testing::TempDir::new();
        let moved = outside.path().join(at.file_name().unwrap());
        std::fs::rename(&at, &moved).unwrap();
        if let Err(e) = crate::caller_hc_tests::link(&moved, &at) {
            assert!(
                std::env::var_os("CI").is_none(),
                "{case}: CI must make links: {e}"
            );
            eprintln!("{case}: skipped, no link: {e}");
            m.w.fakes.hc_done().unwrap();
            continue;
        }
        if case == "dangling exit" {
            std::fs::remove_file(&moved).unwrap();
        }
        let (got, reads) = prove_reads(&m, ID, &r);
        assert_eq!(got, None, "{case}: unproven");
        for n in &never {
            let opened = reads
                .iter()
                .any(|p| p.file_name().is_some_and(|x| x == n.as_str()));
            assert!(!opened, "{case}: {n} opened through the link: {reads:?}");
        }
        m.w.fakes.hc_done().unwrap();
    }
}

// K2, point 8: a FIFO at commit, launcher, client or exit leaves the member unproven and is never opened. Bounded:
// the fakes hold `Rc`, so the proof runs on this thread, and a watchdog thread releases a reader blocked on the FIFO
// after 2 s (a non-blocking writer open; with no reader it fails at once, so the watchdog never blocks either).
#[cfg(unix)]
#[test]
fn a_fifo_in_place_of_lifecycle_evidence_proves_nothing_and_is_never_opened() {
    use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};
    for file in ["commit", "launcher", "client", "exit"] {
        let m = lifecycle_world(0);
        evidence(&m, 0);
        commit(&m, 0, json!({"committed": true}));
        let at = crate::agents::gens::path(&m.folder(ID), 0, file);
        let _ = std::fs::remove_file(&at);
        let c = std::ffi::CString::new(at.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe { libc::mkfifo(c.as_ptr(), 0o600) },
            0,
            "{file}: mkfifo"
        );
        let done = Arc::new(AtomicBool::new(false));
        let (stop, fifo) = (done.clone(), at.clone());
        let dog = std::thread::spawn(move || {
            let start = Instant::now();
            while !stop.load(Ordering::SeqCst) {
                if start.elapsed() > Duration::from_secs(2) {
                    let _ = std::fs::OpenOptions::new()
                        .write(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(&fifo);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let r = m.record(ID);
        let begun = Instant::now();
        let (got, reads) = prove_reads(&m, ID, &r);
        done.store(true, Ordering::SeqCst);
        dog.join().unwrap();
        let name = format!("{file}.json");
        let opened = reads
            .iter()
            .any(|p| p.file_name().is_some_and(|x| x == name.as_str()));
        assert!(!opened, "{file}: the FIFO was opened: {reads:?}");
        assert_eq!(got, None, "{file}: unproven");
        assert!(
            begun.elapsed() < Duration::from_secs(2),
            "{file}: the proof blocked"
        );
        m.w.fakes.hc_done().unwrap();
    }
}

// Point 3: the id and the generation are plain values before any path is joined; a refused value reads nothing,
// the record included. The raw id strings go in as they are. The point-8 walk starts at the member folder, so
// `./<ID>` would read ID's own evidence: only the plain-id check refuses it. `"00"` would name gens/0.
#[test]
fn a_lifecycle_proof_refuses_ids_and_generations_that_are_not_plain_before_reading() {
    let m = lifecycle_world(0);
    evidence(&m, 0);
    commit(&m, 0, json!({"committed": true}));
    let r = m.record(ID);
    let mut zero = r.clone();
    zero["generation"] = json!("00");
    for (case, id, rec) in [
        ("interior dot", format!("./{ID}"), &r),
        ("climbing", format!("../agents/{ID}"), &r),
        ("generation 00", ID.to_string(), &zero),
    ] {
        let (got, reads) = prove_reads(&m, &id, rec);
        assert_eq!(got, None, "{case}");
        assert!(reads.is_empty(), "{case}: nothing read: {reads:?}");
    }
    m.w.fakes.hc_done().unwrap();
}

/// the hc caller ID made the depth-0 `up` leader that the team record names.
pub(crate) fn leader_world() -> Mw {
    let mut m = member_world();
    let team = t::path(&m.state, "$3");
    let mut found = t::read(&team).unwrap().unwrap();
    found.leader = ID.into();
    std::fs::remove_file(&team).unwrap();
    assert!(t::create(&team, &found).unwrap());
    (m.r["depth"], m.r["upTeam"], m.r["parentAgent"]) = (json!(0), json!("t"), Value::Null);
    write(&m.folder(ID).join("record.json"), m.r.clone());
    m.hc();
    m
}
fn placement_calls(m: &Mw) -> Vec<String> {
    m.w.fakes
        .mux_calls()
        .iter()
        .filter(|c| c.iter().any(|a| a == "@mux-agents-placement"))
        .map(|c| c.join(" "))
        .collect()
}

#[test]
fn the_up_leader_sets_and_reads_placement_for_its_own_session() {
    let m = leader_world();
    m.rounds(3, &term(&m.r, Some(150)));
    let (code, out) = crate::cli::run(&["placement", "h"].map(String::from), || Ok(&m.w.ctx));
    assert_eq!(
        (code, out.as_str()),
        (
            0,
            r#"{"ok":true,"verb":"placement","placement":"h","source":"session"}"#
        ),
    );
    assert_eq!(
        placement_calls(&m),
        ["-N set-option -t $3 @mux-agents-placement h"]
    );
    m.w.fakes.hc_done().unwrap();
    m.w.fakes.clear_log();
    m.w.fakes.mux_rule("show-options", 0, "h\n", "");
    m.rounds(3, &term(&m.r, Some(150)));
    let (code, out) = crate::cli::run(&["placement".to_string()], || Ok(&m.w.ctx));
    assert_eq!(
        (code, out.as_str()),
        (
            0,
            r#"{"ok":true,"verb":"placement","placement":"h","source":"session"}"#
        ),
    );
    m.w.fakes.hc_done().unwrap();
}

#[test]
fn every_other_proven_member_is_still_refused_placement_tagged() {
    let tagged = r#"{"ok":false,"verb":"placement","code":2,"check":"tagged","message":"an agent pane cannot choose where agents open"}"#;
    // A depth-1 member, and a depth-0 `up` record that the team record does not name as its leader.
    for case in ["member", "not-the-teams-leader"] {
        let mut m = member_world();
        if case == "not-the-teams-leader" {
            (m.r["depth"], m.r["upTeam"], m.r["parentAgent"]) = (json!(0), json!("t"), Value::Null);
            write(&m.folder(ID).join("record.json"), m.r.clone());
        }
        m.hc();
        m.rounds(3, &term(&m.r, Some(150)));
        for args in [&["placement"][..], &["placement", "h"]] {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            let (code, out) = crate::cli::run(&args, || Ok(&m.w.ctx));
            assert_eq!((code, out.as_str()), (2, tagged), "{case} {args:?}");
            m.rounds(3, &term(&m.r, Some(150)));
        }
        assert!(placement_calls(&m).is_empty(), "{case}");
    }
}

// cascade-3d C2: the gate every spawn passes refuses 2 `closed` under a stopped parent, after the depth check.
#[test]
fn admission_refuses_a_closed_parent() {
    let w = World::human("");
    let c = w.caller();
    team_record(&w, &[]);
    let team = team_of(&w.ctx, &c).unwrap().unwrap();
    let i = ids(1);
    // Lifecycle records: `Tree::read` skips pre-lifecycle members (prelife-1c call 1).
    let fields = json!({"client": "claude", "pane": "%4", "upTeam": "t", "directory": "/work"});
    crate::agents::cascade_fixture::member(&w, LEADER, None, 0, fields.clone());
    let m = crate::agents::cascade_fixture::member(&w, &i[0], Some(LEADER), 1, fields);
    let r = crate::agents::life_fixture::record(&m);
    let s = crate::agents::team_state::Scope::of(&c);
    crate::agents::team_state::open(&w.ctx, &s, |tx| tx.desired(&i[0], false)).unwrap();
    let got = admit(&w.ctx, &c, &team, member_of(&i[0], r)).unwrap_err();
    assert_eq!(got.check, "closed");
    admit(&w.ctx, &c, &team, Standing::Person).unwrap();
    crate::agents::team_state::open(&w.ctx, &s, |tx| tx.desired(LEADER, false)).unwrap();
    let got = admit(&w.ctx, &c, &team, Standing::Person).unwrap_err();
    assert_eq!(got.check, "closed");
}

// Call 10 (C0): S resumed A, so B (A's child) is under S: S may stop and resume B; A, superseded, may not.
#[test]
fn a_successor_stands_for_its_predecessor_in_authority() {
    let i = ids(3);
    let (a, s, b) = (&i[0], &i[1], &i[2]);
    let mut successor = child(s, LEADER, 1);
    successor["resumedFrom"] = json!(a);
    let r: BTreeMap<String, Value> = [
        (LEADER.to_string(), leader_record()),
        (a.clone(), child(a, LEADER, 1)),
        (s.clone(), successor),
        (b.clone(), child(b, a, 2)),
    ]
    .into();
    let t = Tree::from_records(&r);
    assert_eq!(
        t.ancestors(b, LEADER).unwrap(),
        [s.clone(), LEADER.to_string()]
    );
    for verb in ["stop", "resume"] {
        assert!(
            authorize(&t, LEADER, &member_of(s, r[s].clone()), b, verb).is_ok(),
            "{verb}"
        );
        assert_eq!(
            authorize(&t, LEADER, &member_of(a, r[a].clone()), b, verb)
                .unwrap_err()
                .check,
            "authority"
        );
    }
}

// Call 10 through the production readers (records on disk): `authority` and `parent_level` follow S, not A, whose
// recorded client is unknown (a raw walk would refuse `authority` on it).
#[test]
fn authority_and_parent_policy_read_the_tree_from_disk() {
    let w = World::human("");
    let c = w.caller();
    team_record(&w, &[]);
    let team = team_of(&w.ctx, &c).unwrap().unwrap();
    let i = ids(3);
    let (a, s, b) = (&i[0], &i[1], &i[2]);
    cf::member(&w, LEADER, leader_record());
    let mut gone = child(a, LEADER, 1);
    gone["client"] = json!("nonsense");
    cf::member(&w, a, gone);
    let mut successor = child(s, LEADER, 1);
    successor["resumedFrom"] = json!(a);
    let rs = cf::member(&w, s, successor);
    write(
        &w.folder(s).join("spec.json"),
        json!({"flags": ["--effort", "high"]}),
    );
    cf::member(&w, b, child(b, a, 2));
    assert!(
        authority(&w.ctx, &c, &member_of(s, rs.clone()), b, "stop").is_ok(),
        "S stops B"
    );
    let records: BTreeMap<String, Value> = crate::agents::membership::locals(&w.ctx, &c)
        .unwrap()
        .into_iter()
        .map(|l| (l.identity.id, l.stored.record))
        .collect();
    assert_eq!(
        parent_level(&w.ctx, &c, &team, &records, b),
        Ok(t::Policy::Default),
        "B's parent policy is S's"
    );
}

// Call 23 (checklist 7): a pre-lifecycle successor is void exactly when `resume::taken`'s legacy rule says its claim
// never ran (an abandoned claim, or no claim and abort.json); otherwise it stands for A.
#[test]
fn an_abandoned_legacy_successor_is_void() {
    for (case, current) in [
        ("abandoned", false),
        ("aborted", false),
        ("claimed", true),
        ("torn claim", true),
        ("no evidence", true),
    ] {
        let w = World::human("");
        let c = w.caller();
        team_record(&w, &[]);
        let i = ids(3);
        let (a, s, b) = (&i[0], &i[1], &i[2]);
        cf::member(&w, LEADER, leader_record());
        cf::member(&w, a, child(a, LEADER, 1));
        let mut successor = child(s, LEADER, 1);
        successor["resumedFrom"] = json!(a);
        cf::member(&w, s, successor);
        cf::member(&w, b, child(b, a, 2));
        let claim = w.folder(s).join("gens").join("0.claim");
        std::fs::create_dir_all(claim.parent().unwrap()).unwrap();
        match case {
            "abandoned" => write(&claim, json!("abandoned")),
            "aborted" => write(&w.folder(s).join("abort.json"), json!({"at": "x"})),
            "claimed" => write(&claim, json!("run")),
            "torn claim" => std::fs::write(&claim, b"{").unwrap(),
            _ => {}
        }
        let tx = crate::agents::team_state::Tx {
            ctx: &w.ctx,
            scope: crate::agents::team_state::Scope::of(&c),
        };
        let want = if current { s } else { a };
        assert_eq!(
            Tree::all(&tx).unwrap().ancestors(b, LEADER).unwrap(),
            [want.clone(), LEADER.to_string()],
            "{case}"
        );
    }
}

// Call 24 (checklist 8): a present but malformed `parentAgent.id` refuses `authority` in authority, the parent policy
// and a spawn's depth; it is never read as "no parent".
#[test]
fn a_malformed_parent_id_refuses_authority_everywhere() {
    for bad in [json!("../x"), json!(5), json!("")] {
        let w = World::human("");
        let c = w.caller();
        team_record(&w, &[]);
        let team = team_of(&w.ctx, &c).unwrap().unwrap();
        let i = ids(2);
        let (a, b) = (&i[0], &i[1]);
        cf::member(&w, LEADER, leader_record());
        let mut ra = child(a, LEADER, 1);
        ra["parentAgent"] = json!({"id": bad});
        let ra = cf::member(&w, a, ra);
        cf::member(&w, b, child(b, a, 2));
        let records: BTreeMap<String, Value> = crate::agents::membership::locals(&w.ctx, &c)
            .unwrap()
            .into_iter()
            .map(|l| (l.identity.id, l.stored.record))
            .collect();
        let leader = member_of(LEADER, leader_record());
        assert_eq!(
            authority(&w.ctx, &c, &leader, a, "stop").unwrap_err().check,
            "authority",
            "{bad}: authority"
        );
        assert_eq!(
            parent_level(&w.ctx, &c, &team, &records, a)
                .unwrap_err()
                .check,
            "authority",
            "{bad}: policy"
        );
        assert_eq!(
            admit(&w.ctx, &c, &team, member_of(a, ra))
                .unwrap_err()
                .check,
            "authority",
            "{bad}: depth"
        );
    }
}

// Checklist 14: production spawn admission on a successor tree whose predecessor A has no record left, with two
// successors S1 and S2: B is admitted under them at its own depth + 1, and each successor may stop B.
#[test]
fn spawn_admission_follows_several_successors() {
    let w = World::human("");
    let c = w.caller();
    team_record(&w, &[]);
    let team = team_of(&w.ctx, &c).unwrap().unwrap();
    let i = ids(4);
    let (a, s1, s2, b) = (&i[0], &i[1], &i[2], &i[3]);
    cf::member(&w, LEADER, leader_record());
    for s in [s1, s2] {
        let mut r = child(s, LEADER, 1);
        r["resumedFrom"] = json!(a);
        cf::member(&w, s, r);
    }
    let rb = cf::member(&w, b, child(b, a, 2));
    let got = admit(&w.ctx, &c, &team, member_of(b, rb)).unwrap();
    assert_eq!(
        (got.depth, &got.parent["id"]),
        (3, &json!(b)),
        "B admits at depth 3"
    );
    for s in [s1, s2] {
        let rs = crate::state::fs::read_json(&w.folder(s).join("record.json"))
            .unwrap()
            .unwrap();
        assert!(
            authority(&w.ctx, &c, &member_of(s, rs), b, "stop").is_ok(),
            "{s} stops B"
        );
    }
}

// Checklist 14: public resume admission on successor trees. ID resumed A, so X (A's child) is ID's: ID resumes X.
// With a second successor S2 of A, X's parent policy has no one source: refused `authority`, nothing launched.
#[test]
fn a_member_resumes_a_child_of_its_predecessor() {
    let (m, got, x, _) = resume_case("successor");
    let r = m.record(got.unwrap()["agent"]["id"].as_str().unwrap());
    assert_eq!(
        (&r["depth"], &r["resumedFrom"]),
        (&json!(2), &json!(x)),
        "successor"
    );
    m.w.fakes.hc_done().unwrap();
    let (m, got, _, _) = resume_case("successors");
    assert_eq!(
        got.unwrap_err(),
        crate::error::Refusal {
            code: 2,
            check: "authority",
            message: "the parent chain of mdd cannot be read".into(),
            reason: None,
            leftover: None,
            details: Default::default(),
        },
        "successors"
    );
    assert!(
        !m.w.fakes
            .hc_calls()
            .iter()
            .any(|c| c.1.first().is_some_and(|a| a == "run")),
        "successors: no launch"
    );
    m.w.fakes.hc_done().unwrap();
}

// adopt-3e A4, A6, call 13 (findings 2-4, 2-5): a crash after B's change of an adoption that also recomputes C's
// depth. Under the spawn lock, the standing, the record depth and authority are read after replay: C is at 2 under
// B (at 1), B's authority over C holds, and a child of C is admitted at 3.
#[test]
fn admission_reads_after_a_partial_adoption_replays() {
    use crate::agents::{adopt, adopt_fixture as ax, cascade_fixture as cx, team_state::Scope};
    let w = World::human("");
    let c = w.caller();
    team_record(&w, &[]);
    let team = team_of(&w.ctx, &c).unwrap().unwrap();
    cx::member(
        &w,
        LEADER,
        None,
        0,
        json!({"upTeam": "t", "client": "claude"}),
    );
    cx::member(&w, ax::E, Some(LEADER), 1, json!({"client": "claude"}));
    cx::member(&w, cx::B, Some(ax::E), 2, json!({"client": "claude"}));
    cx::member(&w, cx::C, Some(cx::B), 3, json!({"client": "claude"}));
    ax::ended(&w, ax::E);
    let s = Scope::of(&c);
    cx::at("adopt-change-bbbbbbbbbbb1");
    let cut = adopt::adopt_from(&cx::tx(&w, &s), ax::E).unwrap_err();
    crate::agents::release::halt_after(None);
    assert_eq!(cut.check, "halted");
    assert_eq!(ax::depth_of(&w, cx::C), 3, "the crash left C stale");
    let got = with_standing(&w.ctx, &c, |_| {
        let rc = ax::record(&w, cx::C);
        let b = member_of(cx::B, ax::record(&w, cx::B));
        let auth = authority(&w.ctx, &c, &b, cx::C, "stop").map_err(|e| e.check);
        let admitted = admit(&w.ctx, &c, &team, member_of(cx::C, rc.clone()))
            .map(|a| a.depth)
            .map_err(|e| e.check);
        Ok((rc["depth"].clone(), auth, admitted))
    })
    .unwrap();
    assert_eq!(got, (json!(2), Ok(()), Ok(3)));
}
