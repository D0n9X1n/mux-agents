//! The state root from the environment (MuxAgents.ps1:116-123, MuxAgents.Unix.ps1:191-215).

use std::path::PathBuf;

use crate::ctx::Env;
use crate::error::{Refusal, Res, refuse};

/// Which rules pick the root; a parameter so either side's rules test on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsKind {
    Unix,
    Windows,
}

pub const HOST: OsKind = if cfg!(windows) {
    OsKind::Windows
} else {
    OsKind::Unix
};

/// Unix: MUX_AGENTS_STATE (absolute, else refused), else `$XDG_STATE_HOME/mux-agents/state` (absolute only),
/// else `$HOME/.local/state/mux-agents/state`, with realpath applied to its longest existing prefix.
/// Windows: MUX_AGENTS_STATE, else `%LOCALAPPDATA%\mux-agents\state`, from the environment only.
pub fn root(env: &Env, os: OsKind) -> Res<PathBuf> {
    // An empty variable counts as unset, as PowerShell's truthiness test did.
    let var = |name| env.get(name).filter(|v| !v.is_empty());
    match os {
        OsKind::Windows => {
            if let Some(state) = var("MUX_AGENTS_STATE") {
                return Ok(state.into());
            }
            let Some(local) = var("LOCALAPPDATA") else {
                return Err(Refusal::internal("LOCALAPPDATA is not set"));
            };
            // Path.Combine: no separator after one or after a drive colon. Built as text so the Windows rules
            // test on any host.
            let sep = if local.ends_with(['\\', '/', ':']) {
                ""
            } else {
                "\\"
            };
            Ok(format!("{local}{sep}mux-agents\\state").into())
        }
        OsKind::Unix => {
            let path = if let Some(state) = var("MUX_AGENTS_STATE") {
                if !state.starts_with('/') {
                    return refuse(1, "state", "MUX_AGENTS_STATE is not an absolute path");
                }
                state.to_owned()
            } else if let Some(xdg) = var("XDG_STATE_HOME").filter(|x| x.starts_with('/')) {
                format!("{}/mux-agents/state", xdg.trim_end_matches('/'))
            } else if let Some(home) = var("HOME").filter(|h| h.starts_with('/')) {
                format!(
                    "{}/.local/state/mux-agents/state",
                    home.trim_end_matches('/')
                )
            } else {
                return refuse(1, "state", "HOME is not an absolute path");
            };
            real_prefix(&path)
        }
    }
}

/// `path` with realpath applied to its longest existing prefix; the missing rest is kept. `path` must be
/// absolute and free of `.` and `..`, so the kept rest cannot leave the resolved prefix.
pub(crate) fn real_prefix(path: &str) -> Res<PathBuf> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if !path.starts_with('/')
        || path.contains(['\r', '\n'])
        || parts.iter().any(|p| *p == "." || *p == "..")
    {
        return refuse(1, "state", "the state folder path is not absolute");
    }
    let unresolved = || refuse(1, "state", "the state folder path cannot be resolved");
    let prefix = |count: usize| PathBuf::from(format!("/{}", parts[..count].join("/")));
    let mut count = parts.len();
    // try_exists follows links, so a dangling link counts as missing, as Test-Path did.
    loop {
        match prefix(count).try_exists() {
            Ok(true) => break,
            Ok(false) if count > 0 => count -= 1,
            _ => return unresolved(),
        }
    }
    let Ok(mut real) = std::fs::canonicalize(prefix(count)) else {
        return unresolved();
    };
    if !real.is_absolute() || real.to_string_lossy().contains(['\r', '\n']) {
        return unresolved();
    }
    real.extend(&parts[count..]);
    Ok(real)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        Env::from_pairs(pairs.iter().copied())
    }

    fn refused(env: &Env, os: OsKind) -> (u8, &'static str, String) {
        let r = root(env, os).unwrap_err();
        (r.code, r.check, r.message)
    }

    #[cfg(unix)]
    fn state(message: &str) -> (u8, &'static str, String) {
        (1, "state", message.into())
    }

    #[cfg(unix)]
    #[test]
    fn root_prefers_mux_agents_state_then_xdg_then_home() {
        let all = [
            ("MUX_AGENTS_STATE", "/nonexistent-ma/s"),
            ("XDG_STATE_HOME", "/nonexistent-ma/x/"),
            ("HOME", "/nonexistent-ma/h/"),
        ];
        let at = |pairs: &[(&str, &str)]| root(&env(pairs), OsKind::Unix).unwrap();
        assert_eq!(at(&all), PathBuf::from("/nonexistent-ma/s"));
        assert_eq!(
            at(&all[1..]),
            PathBuf::from("/nonexistent-ma/x/mux-agents/state")
        );
        // XDG_STATE_HOME counts only when absolute, as the XDG spec says; empty is unset.
        assert_eq!(
            at(&[("MUX_AGENTS_STATE", ""), ("XDG_STATE_HOME", "rel"), all[2]]),
            PathBuf::from("/nonexistent-ma/h/.local/state/mux-agents/state")
        );
    }

    #[cfg(unix)]
    #[test]
    fn root_relative_state_refuses() {
        assert_eq!(
            refused(
                &env(&[("MUX_AGENTS_STATE", "rel/s"), ("HOME", "/h")]),
                OsKind::Unix
            ),
            state("MUX_AGENTS_STATE is not an absolute path")
        );
        assert_eq!(
            refused(&env(&[("HOME", "rel")]), OsKind::Unix),
            state("HOME is not an absolute path")
        );
        assert_eq!(
            refused(&env(&[]), OsKind::Unix),
            state("HOME is not an absolute path")
        );
        for odd in ["/a/../b", "/a/./b", "/a/b\nc"] {
            assert_eq!(
                refused(&env(&[("MUX_AGENTS_STATE", odd)]), OsKind::Unix),
                state("the state folder path is not absolute"),
                "{odd:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn root_realpaths_longest_existing_prefix() {
        let dir = crate::testing::TempDir::new();
        let base = dir.path();
        let real = base.join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, base.join("link")).unwrap();
        std::os::unix::fs::symlink(base.join("nowhere"), base.join("dangling")).unwrap();
        let spaced = base.join("state ");
        std::fs::create_dir(&spaced).unwrap();
        let of = |state: &std::path::Path| {
            let state = state.to_str().unwrap();
            root(&env(&[("MUX_AGENTS_STATE", state)]), OsKind::Unix).unwrap()
        };
        assert_eq!(of(&base.join("link/s/k")), real.join("s/k"));
        assert_eq!(of(&base.join("link")), real);
        assert_eq!(of(&spaced), spaced);
        // A dangling link does not exist, so its name is kept as the missing rest.
        assert_eq!(of(&base.join("dangling/s")), base.join("dangling/s"));
        let xdg = [
            (
                "XDG_STATE_HOME",
                base.join("link").to_str().unwrap().to_owned(),
            ),
            ("HOME", "/nonexistent".into()),
        ];
        assert_eq!(
            root(&Env::from_pairs(xdg), OsKind::Unix).unwrap(),
            real.join("mux-agents/state")
        );
    }

    #[test]
    fn root_windows_uses_localappdata() {
        let at = |pairs: &[(&str, &str)]| root(&env(pairs), OsKind::Windows).unwrap();
        assert_eq!(
            at(&[("LOCALAPPDATA", r"C:\Users\u\AppData\Local")]),
            PathBuf::from(r"C:\Users\u\AppData\Local\mux-agents\state")
        );
        // Path.Combine adds no separator after one or after a drive colon.
        assert_eq!(
            at(&[("LOCALAPPDATA", r"C:\L\")]),
            PathBuf::from(r"C:\L\mux-agents\state")
        );
        assert_eq!(
            at(&[("LOCALAPPDATA", "D:")]),
            PathBuf::from(r"D:mux-agents\state")
        );
        assert_eq!(
            at(&[("MUX_AGENTS_STATE", r"E:\s"), ("LOCALAPPDATA", r"C:\L")]),
            PathBuf::from(r"E:\s")
        );
        // Names ignore case on Windows.
        assert_eq!(
            at(&[("LocalAppData", r"C:\L")]),
            PathBuf::from(r"C:\L\mux-agents\state")
        );
    }

    #[test]
    fn root_windows_missing_localappdata_is_internal() {
        for pairs in [
            &[][..],
            &[("LOCALAPPDATA", ""), ("MUX_AGENTS_STATE", "")][..],
        ] {
            assert_eq!(
                refused(&env(pairs), OsKind::Windows),
                (
                    1,
                    "internal",
                    "unexpected error (LOCALAPPDATA is not set)".into()
                )
            );
        }
    }
}
