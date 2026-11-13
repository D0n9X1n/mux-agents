//! The injectable hc transport; every native call goes through the bounded process capture.

use std::path::{Path, PathBuf};
use std::time::Duration;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fakes;

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
