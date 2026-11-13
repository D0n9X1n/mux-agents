//! The injectable hc transport; every native call goes through the bounded process capture.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::ctx::Ctx;
use crate::error::{Refusal, Res};
use crate::spawn::Out;

pub trait Hc {
    fn find(&self, name: &str, path: &str) -> Option<PathBuf>;
    fn captured(&self, path: &Path, args: &[&str], timeout: Duration) -> Res<Out>;
}

pub struct NativeHc;

impl Hc for NativeHc {
    fn find(&self, name: &str, path: &str) -> Option<PathBuf> {
        let image = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        crate::mux::find_on_path(&image, path)
    }

    fn captured(&self, path: &Path, args: &[&str], timeout: Duration) -> Res<Out> {
        crate::spawn::captured(path, args, timeout).map_err(|_| Refusal::internal("hc call failed"))
    }
}

const INSTALL: &str =
    "install or upgrade herdr-core to >= 0.1.21 and put hc (or herdr-core) on PATH";

fn failure(message: &str) -> Refusal {
    Refusal {
        code: 1,
        check: "hc",
        message: message.into(),
        reason: None,
    }
}

pub struct Core<'a> {
    provider: &'a dyn Hc,
    bin: PathBuf,
}

pub fn connect(ctx: &Ctx) -> Res<Core<'_>> {
    let path = ctx.env.get("PATH").unwrap_or("");
    let bin = ctx
        .hc
        .find("hc", path)
        .or_else(|| ctx.hc.find("herdr-core", path))
        .ok_or_else(|| failure(INSTALL))?;
    let core = Core {
        provider: ctx.hc.as_ref(),
        bin,
    };
    let out = core
        .provider
        .captured(&core.bin, &["--version"], Duration::from_secs(5))
        .map_err(|_| failure(INSTALL))?;
    if out.code != 0 || !version_ok(&out.stdout) {
        return Err(failure(INSTALL));
    }
    Ok(core)
}

fn version_ok(text: &str) -> bool {
    let mut words = text.split_whitespace();
    if words.next() != Some("hc") {
        return false;
    }
    let Some(v) = words.next() else {
        return false;
    };
    if words.next().is_some() {
        return false;
    }
    let nums = v
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>();
    matches!(nums, Ok(ref n) if n.len() == 3 && (n[0], n[1], n[2]) >= (0, 1, 21))
}

pub struct Run<'a> {
    pub bg: bool,
    pub keep: bool,
    pub outlive_parent: bool,
    pub no_notify: bool,
    pub name: Option<&'a str>,
    pub labels: &'a [(&'a str, &'a str)],
    pub cwd: Option<&'a str>,
    pub argv: &'a [&'a str],
}

fn invalid() -> Refusal {
    failure("hc returned invalid JSON")
}

fn text(v: &Value, key: &str) -> Res<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(invalid)
}

fn address(v: &Value) -> Res<String> {
    text(v, "address").and_then(|a| if a.is_empty() { Err(invalid()) } else { Ok(a) })
}

impl Core<'_> {
    fn call(&self, args: &[String]) -> Res<Out> {
        let argv: Vec<_> = args.iter().map(String::as_str).collect();
        let out = self
            .provider
            .captured(&self.bin, &argv, Duration::from_secs(15))
            .map_err(|_| failure("hc call failed; execution is uncertain; do not retry blindly"))?;
        if out.code != 0 {
            return Err(failure(
                "hc command failed; execution is uncertain; do not retry blindly",
            ));
        }
        Ok(out)
    }

    fn json(&self, args: &[String]) -> Res<Value> {
        serde_json::from_str(&self.call(args)?.stdout).map_err(|_| invalid())
    }

    pub fn run(&self, r: &Run<'_>) -> Res<String> {
        if r.argv.is_empty() {
            return Err(failure("hc run needs argv"));
        }
        if r.argv.len() == 1
            && r.argv[0].contains(|c: char| c.is_whitespace() || ";|&<>$`*?(){}~'\"".contains(c))
        {
            return Err(failure(
                "hc run needs a literal executable, not a shell line",
            ));
        }
        let mut a = vec!["run".into()];
        for (yes, flag) in [
            (r.bg, "--bg"),
            (r.keep, "--keep"),
            (r.outlive_parent, "--outlive-parent"),
            (r.no_notify, "--no-notify"),
        ] {
            if yes {
                a.push(flag.into());
            }
        }
        if let Some(n) = r.name {
            a.extend(["--name".into(), n.into()]);
        }
        for (k, v) in r.labels {
            a.extend(["--label".into(), format!("{k}={v}")]);
        }
        if let Some(c) = r.cwd {
            a.extend(["--cwd".into(), c.into()]);
        }
        a.extend(["--json".into(), "--".into()]);
        a.extend(r.argv.iter().map(|s| s.to_string()));
        let out = self.call(&a)?;
        serde_json::from_str(&out.stdout)
            .map_err(|_| invalid())
            .and_then(|v| address(&v))
            .map_err(|_| {
                failure("hc returned invalid JSON; execution is uncertain; do not retry blindly")
            })
    }

    pub fn kill(&self, t: &str) -> Res<()> {
        self.call(&["kill".into(), t.into(), "--json".into()])
            .map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Live,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    Exited,
    Killed,
    ServerStop,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Foreground {
    pub argv: Vec<String>,
    pub pid: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminal {
    pub address: String,
    pub state: State,
    pub reason: Option<Reason>,
    pub exit_code: Option<i32>,
    pub argv: Vec<String>,
    pub foreground: Option<Foreground>,
    pub labels: std::collections::BTreeMap<String, String>,
    pub pid: Option<u32>,
}

fn string(v: &Value) -> Res<String> {
    v.as_str().map(str::to_owned).ok_or_else(invalid)
}

fn strings(v: &Value) -> Res<Vec<String>> {
    v.as_array()
        .ok_or_else(invalid)?
        .iter()
        .map(string)
        .collect()
}

fn optional<T>(v: &Value, k: &str, f: impl FnOnce(&Value) -> Res<T>) -> Res<Option<T>> {
    match v.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(x) => f(x).map(Some),
    }
}

fn pid(v: &Value) -> Res<u32> {
    v.as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(invalid)
}

impl Terminal {
    fn decode(v: &Value) -> Res<Self> {
        Ok(Self {
            address: address(v)?,
            state: match v["state"].as_str() {
                Some("live") => State::Live,
                Some("finished") => State::Finished,
                _ => return Err(invalid()),
            },
            reason: optional(v, "reason", |x| match x.as_str() {
                Some("exited") => Ok(Reason::Exited),
                Some("killed") => Ok(Reason::Killed),
                Some("server_stop") => Ok(Reason::ServerStop),
                Some(s) => Ok(Reason::Other(s.into())),
                None => Err(invalid()),
            })?,
            exit_code: optional(v, "exit_code", |x| {
                x.as_i64()
                    .and_then(|n| i32::try_from(n).ok())
                    .ok_or_else(invalid)
            })?,
            argv: strings(&v["argv"])?,
            foreground: optional(v, "foreground", |x| {
                Ok(Foreground {
                    argv: strings(&x["argv"])?,
                    pid: x["pid"]
                        .as_i64()
                        .and_then(|n| i32::try_from(n).ok())
                        .ok_or_else(invalid)?,
                })
            })?,
            labels: v["labels"]
                .as_object()
                .ok_or_else(invalid)?
                .iter()
                .map(|(k, v)| Ok((k.clone(), string(v)?)))
                .collect::<Res<_>>()?,
            pid: optional(v, "pid", pid)?,
        })
    }
}

impl Core<'_> {
    pub fn get(&self, t: &str) -> Res<Terminal> {
        Terminal::decode(&self.json(&["get".into(), t.into(), "--json".into()])?)
    }

    pub fn ls(&self, labels: &[(&str, &str)]) -> Res<Vec<String>> {
        let mut a = vec!["ls".into(), "--all".into()];
        for (k, v) in labels {
            a.extend(["--label".into(), format!("{k}={v}")]);
        }
        a.push("--json".into());
        self.json(&a)?["terminals"]
            .as_array()
            .ok_or_else(invalid)?
            .iter()
            .map(address)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fakes;

    fn answer(f: &Fakes, text: &str) {
        f.hc_answer(Ok(Out {
            code: 0,
            stdout: text.into(),
            stderr: String::new(),
        }));
    }

    #[test]
    fn hc_version_order() {
        for (names, want) in [
            (&["hc", "herdr-core"][..], "hc"),
            (&["herdr-core"][..], "herdr-core"),
        ] {
            let f = Fakes::default();
            for n in names {
                f.hc_install(n);
            }
            answer(&f, "hc 0.1.21\n");
            let c = f.ctx();
            assert!(f.hc_calls().is_empty());
            assert_eq!(connect(&c).unwrap().bin, f.hc_install(want));
            assert_eq!(
                f.hc_calls(),
                vec![(
                    f.hc_install(want),
                    vec!["--version".into()],
                    Duration::from_secs(5),
                )]
            );
        }
    }

    #[test]
    fn hc_version_gate() {
        for (v, ok) in [
            ("hc 0.1.3", false),
            ("hc 0.1.20", false),
            ("hc 0.1.21", true),
            ("hc 0.1.100", true),
            ("hc 0.10.0", true),
            ("hc 1.0.0", true),
            ("hc 0.1.21-rc.1", false),
            ("0.1.21", false),
            ("hc 0.1", false),
            ("hc 0.1.21 x", false),
            ("SECRET", false),
        ] {
            let f = Fakes::default();
            let bin = f.hc_install("hc");
            f.hc_install("herdr-core");
            answer(&f, v);
            let c = f.ctx();
            assert_eq!(connect(&c).is_ok(), ok, "{v}");
            assert_eq!(f.hc_calls().len(), 1);
            assert_eq!(f.hc_calls()[0].0, bin);
            if !ok {
                answer(&f, v);
                let e = connect(&c).err().unwrap();
                assert_eq!((e.code, e.check), (1, "hc"));
                assert_eq!(e.message, INSTALL);
                assert!(!e.message.contains("SECRET"));
                assert_eq!(f.hc_calls().len(), 2);
            }
        }
        let f = Fakes::default();
        assert_eq!(connect(&f.ctx()).err().unwrap().message, INSTALL);
        assert!(f.hc_calls().is_empty());
        f.hc_install("hc");
        f.hc_install("herdr-core");
        f.hc_answer(Err(Refusal::internal("SECRET")));
        assert_eq!(connect(&f.ctx()).err().unwrap().message, INSTALL);
        assert_eq!(f.hc_calls().len(), 1);
        f.hc_answer(Ok(Out {
            code: 1,
            stdout: "hc 0.1.21".into(),
            stderr: "SECRET".into(),
        }));
        assert_eq!(connect(&f.ctx()).err().unwrap().message, INSTALL);
        assert_eq!(f.hc_calls().len(), 2);
    }

    #[test]
    fn hc_run_kill() {
        let f = Fakes::default();
        f.hc_install("hc");
        answer(&f, "hc 0.1.21");
        answer(
            &f,
            r#"{"address":"t1","agent":null,"cwd":"/a b","name":"worker","state":"live","turn":0}"#,
        );
        answer(&f, "{}");
        let c = f.ctx();
        let h = connect(&c).unwrap();
        let r = Run {
            bg: true,
            keep: true,
            outlive_parent: true,
            no_notify: true,
            name: Some("worker"),
            labels: &[("team", "x"), ("member", "worker")],
            cwd: Some("/a b"),
            argv: &["claude", "literal ; $()"],
        };
        assert_eq!(h.run(&r).unwrap(), "t1");
        h.kill("t1").unwrap();
        assert_eq!(
            f.hc_calls()[1].1,
            [
                "run",
                "--bg",
                "--keep",
                "--outlive-parent",
                "--no-notify",
                "--name",
                "worker",
                "--label",
                "team=x",
                "--label",
                "member=worker",
                "--cwd",
                "/a b",
                "--json",
                "--",
                "claude",
                "literal ; $()",
            ]
        );
        assert_eq!(f.hc_calls()[2].1, ["kill", "t1", "--json"]);
        assert_eq!(f.hc_calls()[1].2, Duration::from_secs(15));
        assert_eq!(f.hc_calls()[2].2, Duration::from_secs(15));
    }

    #[test]
    fn hc_run_failures() {
        for (case, reply, message) in [
            (
                "transport",
                Err(Refusal::internal("timeout SECRET")),
                "hc call failed; execution is uncertain; do not retry blindly",
            ),
            (
                "nonzero exit",
                Ok(Out {
                    code: 1,
                    stdout: r#"{"address":"t1","detail":"SECRET"}"#.into(),
                    stderr: "SECRET stderr".into(),
                }),
                "hc command failed; execution is uncertain; do not retry blindly",
            ),
            (
                "invalid JSON",
                Ok(Out {
                    code: 0,
                    stdout: "SECRET not JSON".into(),
                    stderr: "SECRET stderr".into(),
                }),
                "hc returned invalid JSON; execution is uncertain; do not retry blindly",
            ),
            (
                "missing address",
                Ok(Out {
                    code: 0,
                    stdout: r#"{"state":"live","detail":"SECRET"}"#.into(),
                    stderr: "SECRET stderr".into(),
                }),
                "hc returned invalid JSON; execution is uncertain; do not retry blindly",
            ),
        ] {
            let f = Fakes::default();
            let bin = f.hc_install("hc");
            answer(&f, "hc 0.1.21");
            // A retry gets the same failure, so the call log must catch it.
            f.hc_answer(reply.clone());
            f.hc_answer(reply);
            let c = f.ctx();
            let h = connect(&c).unwrap();
            let r = Run {
                bg: false,
                keep: false,
                outlive_parent: false,
                no_notify: false,
                name: None,
                labels: &[],
                cwd: None,
                argv: &["claude"],
            };
            let e = h.run(&r).unwrap_err();
            assert_eq!((e.code, e.check), (1, "hc"), "{case}");
            assert_eq!(e.message, message, "{case}");
            assert!(e.message.contains("uncertain"), "{case}");
            assert!(e.message.contains("do not retry blindly"), "{case}");
            assert!(!e.message.contains("SECRET"), "{case}");
            assert_eq!(
                f.hc_calls(),
                vec![
                    (
                        bin.clone(),
                        vec!["--version".into()],
                        Duration::from_secs(5),
                    ),
                    (
                        bin,
                        vec!["run".into(), "--json".into(), "--".into(), "claude".into()],
                        Duration::from_secs(15),
                    ),
                ],
                "{case}"
            );
        }
    }

    #[test]
    fn hc_run_mixed_flags() {
        let f = Fakes::default();
        f.hc_install("hc");
        answer(&f, "hc 0.1.21");
        let c = f.ctx();
        let h = connect(&c).unwrap();
        let mut r = Run {
            bg: true,
            keep: false,
            outlive_parent: false,
            no_notify: true,
            name: None,
            labels: &[],
            cwd: None,
            argv: &["claude"],
        };
        answer(&f, r#"{"address":"t1"}"#);
        assert_eq!(h.run(&r).unwrap(), "t1");
        assert_eq!(
            f.hc_calls()[1].1,
            ["run", "--bg", "--no-notify", "--json", "--", "claude"]
        );
        r.bg = false;
        r.keep = true;
        r.outlive_parent = true;
        r.no_notify = false;
        answer(&f, r#"{"address":"t2"}"#);
        assert_eq!(h.run(&r).unwrap(), "t2");
        assert_eq!(
            f.hc_calls()[2].1,
            [
                "run",
                "--keep",
                "--outlive-parent",
                "--json",
                "--",
                "claude"
            ]
        );
        // Each field alone pins the two pairs' internal wiring as well.
        for (index, flag) in ["--bg", "--keep", "--outlive-parent", "--no-notify"]
            .into_iter()
            .enumerate()
        {
            r.bg = index == 0;
            r.keep = index == 1;
            r.outlive_parent = index == 2;
            r.no_notify = index == 3;
            answer(&f, r#"{"address":"t1"}"#);
            assert_eq!(h.run(&r).unwrap(), "t1");
            assert_eq!(
                f.hc_calls()[index + 3].1,
                ["run", flag, "--json", "--", "claude"]
            );
        }
        assert_eq!(f.hc_calls().len(), 7);
    }

    #[test]
    fn hc_run_options() {
        let f = Fakes::default();
        f.hc_install("hc");
        answer(&f, "hc 0.1.21");
        answer(&f, r#"{"address":"t1"}"#);
        let c = f.ctx();
        let h = connect(&c).unwrap();
        let mut r = Run {
            bg: false,
            keep: false,
            outlive_parent: false,
            no_notify: false,
            name: None,
            labels: &[],
            cwd: None,
            argv: &["claude"],
        };
        assert_eq!(h.run(&r).unwrap(), "t1");
        assert_eq!(f.hc_calls()[1].1, ["run", "--json", "--", "claude"]);
        for argv in [&[][..], &["echo secret; exit"], &["a|b"], &["$(x)"]] {
            r.argv = argv;
            let e = h.run(&r).unwrap_err();
            assert_eq!((e.code, e.check), (1, "hc"));
            assert_eq!(
                e.message,
                if argv.is_empty() {
                    "hc run needs argv"
                } else {
                    "hc run needs a literal executable, not a shell line"
                }
            );
        }
        assert_eq!(f.hc_calls().len(), 2);
        r.argv = &["/usr/local/bin/claude"];
        answer(&f, r#"{"address":"t2"}"#);
        assert_eq!(h.run(&r).unwrap(), "t2");
        assert_eq!(
            f.hc_calls()[2].1,
            ["run", "--json", "--", "/usr/local/bin/claude"]
        );
        assert_eq!(f.hc_calls().len(), 3);
    }

    #[test]
    fn hc_get_rejects_malformed_terminal() {
        for body in [
            r#"{"address":"t1","state":"live","labels":{},"detail":"SECRET"}"#,
            r#"{"address":"t1","state":"live","argv":"SECRET","labels":{}}"#,
        ] {
            let f = Fakes::default();
            f.hc_install("hc");
            answer(&f, "hc 0.1.21");
            f.hc_answer(Ok(Out {
                code: 0,
                stdout: body.into(),
                stderr: "SECRET stderr".into(),
            }));
            let c = f.ctx();
            let e = connect(&c).unwrap().get("t1").unwrap_err();
            assert_eq!((e.code, e.check), (1, "hc"));
            assert_eq!(e.message, "hc returned invalid JSON");
            assert!(!e.message.contains("SECRET"));
            assert_eq!(f.hc_calls().len(), 2);
            assert_eq!(f.hc_calls()[1].1, ["get", "t1", "--json"]);
        }
    }

    #[test]
    fn hc_terminal_fields() {
        for (state, reason, code, argv) in [
            (
                "live",
                Value::Null,
                Value::Null,
                serde_json::json!(["/bin/zsh", "-l"]),
            ),
            (
                "finished",
                serde_json::json!("exited"),
                serde_json::json!(7),
                serde_json::json!(["claude"]),
            ),
            (
                "finished",
                serde_json::json!("killed"),
                Value::Null,
                serde_json::json!([]),
            ),
            (
                "finished",
                serde_json::json!("server_stop"),
                Value::Null,
                serde_json::json!([]),
            ),
            (
                "live",
                serde_json::json!("spawn_pending"),
                Value::Null,
                serde_json::json!([]),
            ),
        ] {
            let f = Fakes::default();
            f.hc_install("hc");
            answer(&f, "hc 0.1.21");
            let wire = serde_json::json!({
                "address": "t1",
                "state": state,
                "reason": reason,
                "exit_code": code,
                "argv": argv,
                "foreground": {"argv": ["child"], "pid": 8, "name": "ignored"},
                "labels": {"team": "x"},
                "pid": 7,
                "future_field": "ignored",
            });
            answer(&f, &wire.to_string());
            let c = f.ctx();
            let t = connect(&c).unwrap().get("t1").unwrap();
            assert_eq!(t.address, "t1");
            assert_eq!(
                t.state,
                if state == "live" {
                    State::Live
                } else {
                    State::Finished
                }
            );
            assert_eq!(
                t.reason,
                match wire["reason"].as_str() {
                    Some("exited") => Some(Reason::Exited),
                    Some("killed") => Some(Reason::Killed),
                    Some("server_stop") => Some(Reason::ServerStop),
                    Some(s) => Some(Reason::Other(s.into())),
                    None => None,
                }
            );
            assert_eq!(t.exit_code, wire["exit_code"].as_i64().map(|n| n as i32));
            assert_eq!(serde_json::json!(t.argv), wire["argv"]);
            assert_eq!(t.pid, Some(7));
            assert_eq!(t.labels["team"], "x");
            assert_eq!(
                t.foreground,
                Some(Foreground {
                    argv: vec!["child".into()],
                    pid: 8,
                })
            );
            assert_eq!(f.hc_calls()[1].1, ["get", "t1", "--json"]);
            assert_eq!(f.hc_calls()[1].2, Duration::from_secs(15));
        }

        let minimal = serde_json::json!({
            "address": "t1",
            "state": "live",
            "argv": [],
            "labels": {},
        });
        for nulls in [false, true] {
            let mut wire = minimal.clone();
            if nulls {
                for key in ["reason", "exit_code", "foreground", "pid"] {
                    wire[key] = Value::Null;
                }
            }
            let t = Terminal::decode(&wire).unwrap();
            assert_eq!(
                (t.reason, t.exit_code, t.foreground, t.pid),
                (None, None, None, None)
            );
        }
        for key in ["address", "state", "argv", "labels"] {
            let mut wire = minimal.clone();
            wire.as_object_mut().unwrap().remove(key);
            assert!(Terminal::decode(&wire).is_err(), "missing {key}");
            wire[key] = Value::Null;
            assert!(Terminal::decode(&wire).is_err(), "null {key}");
        }
        for (key, bad) in [
            ("address", serde_json::json!("")),
            ("address", serde_json::json!(1)),
            ("state", serde_json::json!("unknown")),
            ("state", serde_json::json!(1)),
            ("argv", serde_json::json!([1])),
            ("argv", serde_json::json!("claude")),
            ("labels", serde_json::json!({"team": 1})),
            ("labels", serde_json::json!([])),
            ("reason", serde_json::json!(1)),
            ("exit_code", serde_json::json!("7")),
            ("exit_code", serde_json::json!(i64::from(i32::MAX) + 1)),
            ("exit_code", serde_json::json!(i64::from(i32::MIN) - 1)),
            ("pid", serde_json::json!(-1)),
            ("pid", serde_json::json!("7")),
            ("pid", serde_json::json!(u64::from(u32::MAX) + 1)),
            ("foreground", serde_json::json!({"argv": []})),
            ("foreground", serde_json::json!({"pid": 8})),
            ("foreground", serde_json::json!({"argv": [1], "pid": 8})),
            ("foreground", serde_json::json!({"argv": [], "pid": "8"})),
            (
                "foreground",
                serde_json::json!({"argv": [], "pid": i64::from(i32::MAX) + 1}),
            ),
        ] {
            let mut wire = minimal.clone();
            wire[key] = bad;
            let e = Terminal::decode(&wire).unwrap_err();
            assert_eq!((e.code, e.check), (1, "hc"), "{wire}");
            assert_eq!(e.message, "hc returned invalid JSON", "{wire}");
        }
    }

    #[test]
    fn hc_protocol_list() {
        let f = Fakes::default();
        f.hc_install("hc");
        answer(&f, "hc 0.1.21");
        answer(
            &f,
            r#"{"terminals":[{"address":"t1","state":"live"},{"address":"t2","state":"finished"}],"hidden_finished":0}"#,
        );
        let c = f.ctx();
        let h = connect(&c).unwrap();
        assert_eq!(
            h.ls(&[("team", "x"), ("member", "y")]).unwrap(),
            ["t1", "t2"]
        );
        assert_eq!(
            f.hc_calls()[1].1,
            [
                "ls", "--all", "--label", "team=x", "--label", "member=y", "--json"
            ]
        );
        assert_eq!(f.hc_calls()[1].2, Duration::from_secs(15));
        answer(&f, r#"{"terminals":[]}"#);
        assert!(h.ls(&[]).unwrap().is_empty());
        assert_eq!(f.hc_calls()[2].1, ["ls", "--all", "--json"]);
    }

    #[test]
    fn hc_protocol_failures() {
        for (code, body) in [
            (1, "SECRET"),
            (0, "SECRET"),
            (0, "{}"),
            (0, r#"{"terminals":[{"address":""}]}"#),
            (0, "null"),
            (0, "[]"),
            (0, r#"{"terminals":null}"#),
            (0, r#"{"terminals":{}}"#),
            (0, r#"{"terminals":[{}]}"#),
            (0, r#"{"terminals":[{"address":1}]}"#),
            (0, r#"{"terminals":[{"address":"t1"},null]}"#),
        ] {
            let f = Fakes::default();
            f.hc_install("hc");
            answer(&f, "hc 0.1.21");
            f.hc_answer(Ok(Out {
                code,
                stdout: body.into(),
                stderr: "SECRET".into(),
            }));
            let c = f.ctx();
            let e = connect(&c).unwrap().ls(&[]).unwrap_err();
            assert_eq!((e.code, e.check), (1, "hc"));
            assert!(!e.message.contains("SECRET"));
            if code != 0 {
                assert!(e.message.contains("do not retry blindly"));
            } else {
                assert_eq!(e.message, "hc returned invalid JSON");
            }
            assert_eq!(f.hc_calls().len(), 2);
        }
        let f = Fakes::default();
        f.hc_install("hc");
        answer(&f, "hc 0.1.21");
        f.hc_answer(Err(Refusal::internal("timeout SECRET")));
        let c = f.ctx();
        let e = connect(&c).unwrap().kill("t1").unwrap_err();
        assert!(e.message.contains("do not retry blindly"));
        assert!(!e.message.contains("SECRET"));
        assert_eq!(f.hc_calls().len(), 2);
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"address": "t1", "state": "unknown", "argv": [], "labels": {}}),
            serde_json::json!({"address": "t1", "state": "live", "argv": [1], "labels": {}}),
        ] {
            assert!(Terminal::decode(&bad).is_err());
        }
        let minimal =
            serde_json::json!({"address": "t1", "state": "live", "argv": [], "labels": {}});
        let t = Terminal::decode(&minimal).unwrap();
        assert_eq!(
            (t.pid, t.foreground, t.exit_code, t.reason),
            (None, None, None, None)
        );
    }

    #[test]
    fn hc_fake_records() {
        let f = Fakes::default();
        let p = f.hc_install("hc");
        let c = f.ctx();
        f.hc_answer(Ok(Out {
            code: 0,
            stdout: "hc 0.1.21".into(),
            stderr: String::new(),
        }));
        assert_eq!(c.hc.find("hc", ""), Some(p.clone()));
        assert_eq!(
            c.hc.captured(&p, &["--version"], Duration::from_secs(5))
                .unwrap()
                .stdout,
            "hc 0.1.21"
        );
        assert_eq!(
            f.hc_calls(),
            vec![(p, vec!["--version".to_string()], Duration::from_secs(5))]
        );
        assert!(c.hc.captured(Path::new("hc"), &[], Duration::ZERO).is_err());
    }
}
