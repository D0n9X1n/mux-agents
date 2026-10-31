//! The providers every verb runs against (the old `$P`): environment, identity, paths and the OS, mux and clock seams.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::mux::Mux;
use crate::proc::Os;

pub trait Clock {
    fn now(&self) -> SystemTime;
    fn sleep(&self, duration: Duration);
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// The process environment in its original order.
#[derive(Debug, Clone, Default)]
pub struct Env {
    vars: Vec<(String, String)>,
}

impl Env {
    pub fn from_pairs<K: Into<String>, V: Into<String>>(
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        Env {
            vars: pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    /// Non-UTF-8 names and values are read lossily; none of the variables mux-agent reads can hold such bytes.
    pub fn from_os(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        Env::from_pairs(vars.into_iter().map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        }))
    }

    /// Exact name first, else the first name equal ignoring case (Windows names ignore case; MuxAgents.ps1:308).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars
            .iter()
            .find(|(k, _)| k == name)
            .or_else(|| self.vars.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)))
            .map(|(_, v)| v.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.vars.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

pub struct Ctx {
    pub env: Env,
    pub pid: u32,
    pub cwd: PathBuf,
    pub home: PathBuf,
    pub state_root: PathBuf,
    pub exe: PathBuf,
    pub os: Box<dyn Os>,
    // Resolved on first use, so verbs that never reach the multiplexer never probe for it.
    pub mux: RefCell<Option<Box<dyn Mux>>>,
    pub clock: Box<dyn Clock>,
    // The launcher's pane already passed the sandbox check when it was spawned.
    pub admitted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn env_lookup_prefers_exact_then_ignores_case() {
        let env = Env::from_pairs([("Path", "upper"), ("PATH", "exact"), ("Home", "folded")]);
        assert_eq!(env.get("PATH"), Some("exact"));
        assert_eq!(env.get("path"), Some("upper"));
        assert_eq!(env.get("HOME"), Some("folded"));
        assert_eq!(env.get("MISSING"), None);
    }

    #[test]
    fn fake_os_and_mux_record_calls() {
        let fakes = testing::Fakes::default();
        fakes.process(testing::row(42, 1, 4200));
        fakes.mux_rule("kill-pane", 1, "", "SECRET");
        let ctx = fakes.ctx();

        assert_eq!(ctx.os.row(42), Some(testing::row(42, 1, 4200)));
        assert_eq!(ctx.os.row(7), None);
        let mux = ctx.mux.borrow();
        let mux = mux.as_ref().unwrap();
        let listed = mux.run(&["-N", "list-panes", "-a"]).unwrap();
        assert_eq!(listed.code, 0);
        let killed = mux.run(&["-N", "kill-pane", "-t", "%9"]).unwrap();
        assert_eq!((killed.code, killed.stderr.as_str()), (1, "SECRET"));
        ctx.clock.sleep(std::time::Duration::from_millis(250));

        assert_eq!(fakes.os_calls(), ["row 42", "row 7"]);
        assert_eq!(
            fakes.mux_calls(),
            [
                vec!["-N", "list-panes", "-a"],
                vec!["-N", "kill-pane", "-t", "%9"]
            ]
        );
        assert_eq!(fakes.slept(), [250]);
    }
}
