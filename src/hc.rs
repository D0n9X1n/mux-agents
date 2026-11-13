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
        address(&self.json(&a)?)
    }

    pub fn kill(&self, t: &str) -> Res<()> {
        self.call(&["kill".into(), t.into(), "--json".into()])
            .map(|_| ())
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
            ("hc 0.1.20", false),
            ("hc 0.1.21", true),
            ("hc 0.10.0", true),
            ("hc 1.0.0", true),
            ("hc 0.1.21-rc.1", false),
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
            labels: &[("team", "x")],
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
        for argv in [&[][..], &["echo secret; exit"]] {
            r.argv = argv;
            assert!(h.run(&r).is_err());
        }
        assert_eq!(f.hc_calls().len(), 2);
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
