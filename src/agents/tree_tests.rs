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
use serde_json::Map;
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
    prove_member(&m.w.ctx, &crate::agents::membership::team(&m.c), id, &r, &t)
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
    let i = ids(6);
    let parents = [LEADER, i[0].as_str(), i[1].as_str(), LEADER, LEADER, LEADER];
    cf::member(&w, LEADER, leader_record());
    for (n, id) in i.iter().enumerate() {
        cf::member(
            &w,
            id,
            child(id, parents[n], if n < 3 { n as u64 + 1 } else { 1 }),
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
    let (five, budget) = (
        snap(&rows, 0),
        crate::agents::budget::path(&w.state(), "$3"),
    );
    let taken = place(&w.ctx, &c, &a, &five, Client::Claude, None, || {
        refuse(2, "name", "taken")
    });
    assert_eq!(taken.unwrap_err().check, "name");
    assert!(!budget.exists(), "a name refusal spends no token");
    write(&budget, json!({"tokens": 0, "at": 0}));
    assert_eq!(
        place(&w.ctx, &c, &a, &five, Client::Claude, None, kid)
            .unwrap_err()
            .check,
        "budget",
        "budget last"
    );
    let pa = admit(&w.ctx, &c, &team, Standing::Person).unwrap();
    let p = place(&w.ctx, &c, &pa, &five, Client::Claude, None, kid).unwrap();
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
    let m = place(&w.ctx, &c, &a, &five, Client::Claude, None, kid).unwrap();
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
        ancestors(&r, &i[1]).unwrap(),
        [i[0].clone(), LEADER.to_string()]
    );
    assert!(ancestors(&r, LEADER).unwrap().is_empty());
    r.insert(i[2].clone(), child(&i[2], "cccccccccccc", 2));
    assert_eq!(
        ancestors(&r, &i[2]),
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
            ancestors(&r, &i[1]).unwrap_err().check,
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
fn of_two_racing_spawns_at_six_one_wins() {
    let w = World::human("");
    team_record(&w, &[]);
    cf::member(&w, LEADER, leader_record());
    for id in ids(5) {
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
            let all = crate::agents::resume::every_record(&r.ctx)?;
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
            authorize(&r, &m(who, &r), &i[1], "stop").is_ok(),
            "{who}: an ancestor"
        );
    }
    assert!(
        authorize(&r, &Standing::Person, &i[1], "read").is_ok(),
        "a person"
    );
    for who in [&i[4], &i[2], &i[3], &i[1]] {
        assert_eq!(
            authorize(&r, &m(who, &r), &i[1], "read"),
            refuse(2, "authority", "only an ancestor or a person can read ma1"),
            "{who}: sibling, peer, cousin, self"
        );
    }
    assert_eq!(
        authorize(&r, &m(&i[1], &r), &i[0], "stop")
            .unwrap_err()
            .check,
        "authority",
        "a child"
    );
    r.insert(i[1].clone(), child(&i[1], "cccccccccccc", 2));
    assert_eq!(
        authorize(&r, &m(&i[0], &r), &i[1], "stop"),
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
/// `held`: whether the team lock was held at each `hc get` (recorded in the "candidate" case, run through the CLI).
fn resume_case(case: &str) -> (Mw, Res<Map<String, Value>>, String, Held) {
    let mut m = member_world();
    let x = "dddddddddddd".to_string();
    let mut rx = child(&x, if case == "non-descendant" { OTHER } else { ID }, 2);
    rx["sessionId"] = "22222222-2222-4222-8222-222222222222".into();
    let rx = cf::member(&m.w, &x, rx);
    let bypass = case == "parent-bound" || case == "team-bound";
    write(
        &m.folder(&x).join("spec.json"),
        json!({"flags": if bypass { json!(["--dangerously-skip-permissions"]) } else { json!([]) }}),
    );
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
    if case == "limit" {
        for id in ids(4) {
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
    if case == "ok" || cand {
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
    let rx = cf::member(&w, x, rx);
    write(&w.folder(x).join("spec.json"), json!({"flags": []}));
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
/// A person's resume where limit, ceiling and name all fail: v5 order gives limit, then ceiling, then name.
#[test]
fn resume_refuses_in_v5_order() {
    for (more, bypass, check) in [
        (5, false, "limit"),
        (4, false, "ceiling"),
        (4, true, "name"),
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
        let rx = cf::member(&w, x, rx);
        write(
            &w.folder(x).join("spec.json"),
            json!({"flags": ["--dangerously-skip-permissions"]}),
        );
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
/// Through the CLI: a candidate whose proof fails refuses 2 `tagged` and makes no folder.
#[test]
fn a_candidate_resume_is_refused_with_no_side_effects() {
    let mut m = member_world();
    m.w.fakes.forget(170);
    let before = std::fs::read_dir(m.state.join("agents")).unwrap().count();
    m.hc();
    m.rounds(3, &term(&m.r, Some(150)));
    let (code, out) = crate::cli::run(&["resume", "mbb"].map(String::from), || Ok(&m.w.ctx));
    assert_eq!(
        (code, out.contains(r#""check":"tagged""#)),
        (2, true),
        "{out}"
    );
    assert_eq!(
        std::fs::read_dir(m.state.join("agents")).unwrap().count(),
        before
    );
    m.w.fakes.hc_done().unwrap();
}
/// A request refused by itself spends no token (#103 D5): a proven member's brief child with no -BriefFile refuses 2
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
/// debit is the last step before `hc run` (#103 D5).
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
