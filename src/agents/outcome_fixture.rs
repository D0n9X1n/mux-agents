//! outcome-2's fixture (#103): one lifecycle member `ID`, generation 1 launched at `T0` (spec.json deadline T0 + 150 s,
//! launch.json), launcher 100 (start 1000) and its client 150 (start 1500) in the fake process table. Every file is
//! written through `gens::publish_once`, the A3b publisher.
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use super::fixture::{ID, World};
use super::gens::{self, publish_once};
use crate::state::evidence::Evidence;
use crate::testing::row;
use crate::util::{parse_utc, utc_o};

pub const T0: &str = "2026-11-01T12:00:00.0000000Z";
pub const HC: &str = "m00000001";
pub const SID: &str = "0f11fc2f-2032-48ef-a920-4af2a8e5af50";
pub const LAUNCHER: [&str; 2] = ["mux-agent", "__launch"];

pub struct G {
    pub w: World,
    pub key: String,
    pub folder: PathBuf,
}

pub fn world() -> G {
    let w = World::human("");
    w.clock(T0);
    let key = w.caller().server_key;
    assert!(
        gens::plain_key(&key),
        "the fixture's server key is plain: {key}"
    );
    let folder = w.agent_record(ID, json!({"lifecycle": 1, "id": ID, "generation": "1"}));
    w.fakes.process(row(100, 1, 1000));
    w.fakes.process(row(150, 100, 1500));
    let g = G { w, key, folder };
    g.launched(1);
    g
}

impl G {
    /// T0 + `secs`, as `utc_o` writes it.
    pub fn at(&self, secs: u64) -> String {
        utc_o(parse_utc(T0).unwrap() + Duration::from_secs(secs))
    }
    pub fn put(&self, n: u64, file: &str, body: Value) {
        publish_once(&self.folder, n, file, &body).unwrap();
    }
    /// launch-1 G1.2/G12: generation n's spec.json (deadline T0 + 150 s) and launch.json.
    pub fn launched(&self, n: u64) {
        let spec = json!({"id": ID, "generation": n.to_string(), "sessionId": SID, "deadline": self.at(150)});
        self.put(n, "spec", spec);
        self.put(
            n,
            "launch",
            json!({"hcName": HC, "launcherArgv": LAUNCHER, "sessionId": SID}),
        );
    }
    pub fn claim(&self, n: u64, body: Value) {
        gens::publish_claim_once(&self.folder, n, &body).unwrap();
    }
    /// The launcher's won claim `run`, the commit `committed`, launcher.json of launcher 100.
    pub fn ran(&self, n: u64) {
        self.claim(n, json!("run"));
        self.put(n, "commit", json!({"committed": true}));
        self.put(
            n,
            "launcher",
            json!({"pid": 100, "start": 1000, "startedAt": T0}),
        );
    }
    /// K4's report.json naming `pid` (start 1500).
    pub fn report(&self, n: u64, pid: u32) {
        self.put(
            n,
            "report",
            json!({"sessionId": SID, "pid": pid, "start": 1500, "at": T0}),
        );
    }
    /// K1's client.json of client 150.
    pub fn client(&self, n: u64) {
        self.put(n, "client", json!({"pid": 150, "start": 1500}));
    }
    /// A whole file, None when absent; an unreadable one fails the test.
    pub fn file(&self, n: u64, file: &str) -> Option<Value> {
        match gens::read(&self.folder, n, file) {
            Evidence::Present(v) => Some(v),
            Evidence::Absent => None,
            Evidence::Unreadable => panic!("gens/{n}/{file}.json is unreadable"),
        }
    }
}
