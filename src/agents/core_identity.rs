use super::registry::{Launch, Pane, Row};
use crate::hc::Terminal;
use serde_json::Value;
pub fn generation(g: &str) -> bool {
    !g.is_empty()
        && g.len() <= 64
        && g.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub address: String,
    pub generation: String,
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
            team: team.into(),
            session_id: s("sessionId")?.into(),
            launcher,
        })
    }
    pub fn matches(&self, t: &Terminal) -> bool {
        t.address == self.address
            && [
                ("mux.managed", "1"),
                ("mux.team", self.team.as_str()),
                ("mux.id", self.id.as_str()),
                ("mux.gen", self.generation.as_str()),
            ]
            .iter()
            .all(|(k, v)| t.labels.get(*k).map(String::as_str) == Some(*v))
    }
    pub fn view(&self, r: &Value, rows: &[Row]) -> Option<Pane> {
        let pane = r.get("pane")?.as_str()?;
        let command = r.get("startCommand")?.as_str()?;
        if command.is_empty() {
            return None;
        }
        let mut found = rows.iter().filter(|row| row.pane == pane);
        let row = found.next()?;
        if found.next().is_some()
            || row.dead
            || row.command != command
            || row.pid.parse::<u32>().ok()? < 2
            || !row.window.starts_with('@')
        {
            return None;
        }
        Some(Pane {
            pane: row.pane.clone(),
            window: row.window.clone(),
            pid: row.pid.clone(),
            id: self.id.clone(),
            name: self.name.clone(),
            record: r.clone(),
            launch: Launch {
                command: command.into(),
                pid: None,
            },
        })
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
            id.view(&r, std::slice::from_ref(&row)).unwrap().id,
            "aaaaaaaaaaaa"
        );
        assert!(id.view(&r, &[]).is_none());
        assert!(id.view(&r, &[row.clone(), row.clone()]).is_none());
        let mut changed = row.clone();
        changed.command = "human shell".into();
        assert!(id.view(&r, &[changed]).is_none());
        for pid in ["1", "0", ""] {
            let mut invalid = row.clone();
            invalid.pid = pid.into();
            assert!(id.view(&r, &[invalid]).is_none(), "pid {pid:?}");
        }
        let mut invalid_window = row.clone();
        invalid_window.window = "1".into();
        assert!(id.view(&r, &[invalid_window]).is_none(), "window without @");
        let mut dead = row;
        dead.dead = true;
        assert!(id.view(&r, &[dead]).is_none());
    }
}
