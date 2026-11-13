use super::{
    core_identity::Identity,
    lifecycle::{Life, classify},
};
use crate::{
    error::{Refusal, Res},
    hc::{Core, Terminal},
};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone)]
pub struct Observation {
    pub identity: Identity,
    pub terminal: Option<Terminal>,
    pub life: Life,
}
#[derive(Debug)]
pub struct Snapshot {
    pub local: Vec<Observation>,
    pub reserved: Vec<(String, String)>,
}
impl Snapshot {
    pub fn count(&self) -> usize {
        self.local.iter().filter(|o| o.life != Life::Ended).count() + self.reserved.len()
    }
    pub fn names(&self) -> Vec<String> {
        self.local
            .iter()
            .filter(|o| o.life != Life::Ended)
            .map(|o| o.identity.name.clone())
            .chain(self.reserved.iter().map(|(_, n)| n.clone()))
            .collect()
    }
}
pub fn take(core: &Core<'_>, team: &str, members: &[Identity]) -> Res<Snapshot> {
    let listed = core.ls(&[("mux.managed", "1"), ("mux.team", team)])?;
    let addresses: BTreeSet<String> = listed
        .iter()
        .cloned()
        .chain(members.iter().map(|i| i.address.clone()))
        .collect();
    let facts: BTreeMap<String, Option<Terminal>> = addresses
        .into_iter()
        .map(|a| {
            let t = core.get(&a).ok();
            (a, t)
        })
        .collect();
    let local = members
        .iter()
        .map(|id| {
            let terminal = facts
                .get(&id.address)
                .cloned()
                .flatten()
                .filter(|t| id.matches(t));
            let life = classify(terminal.as_ref(), &id.launcher);
            Observation {
                identity: id.clone(),
                terminal,
                life,
            }
        })
        .collect();
    let mut reserved = Vec::new();
    for address in listed.into_iter().collect::<BTreeSet<_>>() {
        if members.iter().any(|i| i.address == address) {
            continue;
        }
        let t = facts
            .get(&address)
            .and_then(Option::as_ref)
            .ok_or_else(|| Refusal::internal("cannot verify cross-root reservations"))?;
        if t.address != address
            || t.labels.get("mux.managed").map(String::as_str) != Some("1")
            || t.labels.get("mux.team").map(String::as_str) != Some(team)
        {
            return Err(Refusal::internal("invalid cross-root reservation"));
        }
        if classify(Some(t), &[]) == Life::Ended {
            continue;
        }
        let name = t
            .labels
            .get("mux.name")
            .filter(|n| super::agent_name(n))
            .ok_or_else(|| Refusal::internal("reservation has no managed name"))?;
        reserved.push((address, name.clone()));
    }
    Ok(Snapshot { local, reserved })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{agents::core_identity::tests::identity, hc, spawn::Out, testing::Fakes};
    use serde_json::json;
    fn answer(f: &Fakes, v: serde_json::Value) {
        f.hc_answer(Ok(Out {
            stdout: v.to_string(),
            ..Out::default()
        }));
    }
    fn connect(f: &Fakes) {
        f.hc_install("hc");
        f.hc_answer(Ok(Out {
            stdout: "hc 0.1.21".into(),
            ..Out::default()
        }));
    }
    fn wire(id: &Identity) -> serde_json::Value {
        json!({"address":id.address,"state":"live","argv":id.launcher,"labels":{"mux.managed":"1","mux.team":id.team,"mux.id":id.id,"mux.gen":id.generation,"mux.name":id.name}})
    }
    #[test]
    fn snapshot_gets_each_address_once_and_failures_are_unknown() {
        for state in [
            "running",
            "lost",
            "ended",
            "missing",
            "mismatch",
            "server_stop",
        ] {
            let f = Fakes::default();
            connect(&f);
            let ctx = f.ctx();
            let core = hc::connect(&ctx).unwrap();
            let id = identity();
            answer(&f, json!({"terminals":[{"address":"t1"},{"address":"t1"}]}));
            let mut v = wire(&id);
            match state {
                "lost" => v["argv"] = json!(["/bin/zsh", "-l"]),
                "ended" => {
                    v["state"] = json!("finished");
                    v["reason"] = json!("exited");
                    v["exit_code"] = json!(7);
                }
                "mismatch" => v["labels"]["mux.gen"] = json!("1"),
                "server_stop" => {
                    v["state"] = json!("finished");
                    v["reason"] = json!("server_stop");
                }
                _ => {}
            }
            if state == "missing" {
                f.hc_answer(Err(crate::error::Refusal::internal("gone")));
            } else {
                answer(&f, v);
            }
            let s = take(&core, "k:$3", &[id]).unwrap();
            let want = match state {
                "running" => Life::Running,
                "lost" => Life::Lost,
                "ended" => Life::Ended,
                _ => Life::Unknown,
            };
            assert_eq!(s.local[0].life, want, "{state}");
            assert_eq!(s.count(), usize::from(want != Life::Ended));
            assert_eq!(
                f.hc_calls()
                    .iter()
                    .filter(|(_, a, _)| a[0] == "get")
                    .count(),
                1
            );
            assert_eq!(
                s.names().contains(&"worker".to_string()),
                want != Life::Ended
            );
        }
    }
}
