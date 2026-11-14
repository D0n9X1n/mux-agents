use super::*;
use crate::agents::{
    core_facts, core_identity,
    fixture::{World, write},
    membership::Stored,
    team as t,
};
use crate::parse::Client;
use serde_json::json;
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
