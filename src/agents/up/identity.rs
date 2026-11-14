//! T = sha16(canonical state root|canonical folder|client|name) (#103). Links and case aliases are one folder.

use std::path::{Path, PathBuf};

use crate::{ctx::Ctx, error::Res, parse::Client};

pub fn folder(ctx: &Ctx) -> Res<PathBuf> {
    Ok(std::fs::canonicalize(&ctx.cwd)?)
}

/// The privacy helper makes a missing root (0700 on Unix; never create_dir_all, which umask 022 makes 0755), so a
/// root missing before the first run gives the T of the later runs.
pub fn team(ctx: &Ctx, folder: &Path, client: Client, name: &str) -> Res<String> {
    #[cfg(unix)]
    crate::state::private_unix::private_dirs(&ctx.state_root, &[&ctx.state_root])?;
    #[cfg(windows)]
    crate::state::private_windows::private_dirs(&[&ctx.state_root])?;
    let (root, client) = (
        std::fs::canonicalize(&ctx.state_root)?,
        crate::agents::spawn::client_name(client),
    );
    Ok(crate::util::sha256_hex(format!(
        "{}|{}|{client}|{name}",
        root.display(),
        folder.display()
    ))[..16]
        .to_string())
}

/// `<basename, [A-Za-z0-9_-] kept, others _>-<T[..8]>-<32 random hex>`.
pub fn session_name(folder: &Path, team: &str) -> Res<String> {
    let keep = |c: char| {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            c
        } else {
            '_'
        }
    };
    let base: String = folder
        .file_name()
        .map(|n| n.to_string_lossy().chars().map(keep).collect())
        .unwrap_or_default();
    Ok(format!("{base}-{}-{}", &team[..8], crate::util::hex32()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Fakes, TempDir};

    fn t(root: &Path, cwd: &Path, client: Client, name: &str) -> String {
        let mut c = Fakes::default().ctx();
        c.state_root = root.into();
        c.cwd = cwd.into();
        team(&c, &folder(&c).unwrap(), client, name).unwrap()
    }

    #[test]
    fn identity_is_one_root_folder_client_and_name_and_aliases_are_one() {
        let d = TempDir::new();
        let dir = d.path().join("Proj");
        std::fs::create_dir_all(&dir).unwrap();
        let (s1, s2) = (d.path().join("s1"), d.path().join("s2"));
        let first = t(&s1, &dir, Client::Claude, "");
        assert!(s1.is_dir() && first.len() == 16 && t(&s1, &dir, Client::Claude, "") == first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&s1).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        for other in [
            t(&s2, &dir, Client::Claude, ""),
            t(&s1, &dir, Client::Copilot, ""),
            t(&s1, &dir, Client::Claude, "b"),
        ] {
            assert_ne!(other, first);
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&dir, d.path().join("alias")).unwrap();
            assert_eq!(t(&s1, &d.path().join("alias"), Client::Claude, ""), first);
        }
        if d.path().join("PROJ").exists() {
            assert_eq!(t(&s1, &d.path().join("PROJ"), Client::Claude, ""), first);
        }
        let n = session_name(Path::new("/x/project.v1"), "0123456789abcdef").unwrap();
        assert!(
            n.starts_with("project_v1-01234567-") && n.len() == "project_v1-01234567-".len() + 32
        );
    }
}
