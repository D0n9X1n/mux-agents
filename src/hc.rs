//! The injectable hc transport; every native call goes through the bounded process capture.

use std::path::{Path, PathBuf};
use std::time::Duration;

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
