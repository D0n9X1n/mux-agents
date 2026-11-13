//! Explicit launcher context preserves the core's inherited endpoints.

use std::path::Path;

use crate::ctx::{Ctx, Env};
use crate::error::{Res, refuse};

/// The executable and six-argument launcher form for hc, carrying state root and PATH but no caller HC endpoints.
/// Tests only until the hc launch path calls it in phase 1b-ii-b2.
pub fn argv(ctx: &Ctx, file: &Path) -> Res<Vec<String>> {
    use crate::agents::spawn::utf8;
    Ok([
        utf8(&ctx.exe)?,
        "launcher",
        utf8(file)?,
        "--state-root",
        utf8(&ctx.state_root)?,
        "--path",
        ctx.env.get("PATH").unwrap_or(""),
    ]
    .map(str::to_owned)
    .into())
}

/// The spec from only `launcher <spec>` or `launcher <spec> --state-root <abs> --path <value>`.
/// Matches the exact grammar; `environment` checks that the state folder is absolute.
pub fn spec(args: &[String]) -> Option<&str> {
    match args {
        [v, s] if v == "launcher" => Some(s),
        [v, s, r, _, p, _] if v == "launcher" && r == "--state-root" && p == "--path" => Some(s),
        _ => None,
    }
}

/// Whether the arguments match exactly the six-argument launcher form with explicit state root and PATH.
pub fn is_scoped(args: &[String]) -> bool {
    args.len() == 6 && spec(args).is_some()
}

/// The two-argument form keeps the inherited environment unchanged; the six-argument form replaces only PATH and MUX_AGENTS_STATE.
/// A relative state folder refuses with code 1 and check `state`, as in `state::root`; malformed arguments refuse usage.
pub fn environment(args: &[String], inherited: &Env) -> Res<Env> {
    if spec(args).is_none() {
        return refuse(2, "usage", "invalid launcher arguments");
    }
    if args.len() == 2 {
        return Ok(inherited.clone());
    }
    if !Path::new(&args[3]).is_absolute() {
        return refuse(1, "state", "the state folder path is not absolute");
    }
    let kept = inherited
        .iter()
        .filter(|(k, _)| {
            !k.eq_ignore_ascii_case("PATH") && !k.eq_ignore_ascii_case("MUX_AGENTS_STATE")
        })
        .map(|(k, v)| (k.to_owned(), v.to_owned()));
    Ok(Env::from_pairs(kept.chain([
        ("MUX_AGENTS_STATE".into(), args[3].clone()),
        ("PATH".into(), args[5].clone()),
    ])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::fixture::World;
    use crate::ctx::Env;

    #[test]
    fn scope_is_explicit_and_keeps_core_endpoints() {
        let mut w = World::human("");
        w.ctx.env = Env::from_pairs([
            ("PATH", "client path;literal"),
            ("HC_SOCKET", "caller-socket"),
        ]);
        let file = w.ctx.state_root.join("s/agents/a/spec.json");
        let a = argv(&w.ctx, &file).unwrap();
        assert_eq!(
            a,
            vec![
                w.ctx.exe.to_str().unwrap(),
                "launcher",
                file.to_str().unwrap(),
                "--state-root",
                w.ctx.state_root.to_str().unwrap(),
                "--path",
                "client path;literal"
            ]
        );
        assert!(!a.iter().any(|s| s.contains("caller-socket")));
        let inherited = Env::from_pairs([
            ("PATH", "core-path"),
            ("MUX_AGENTS_STATE", "core-root"),
            ("HC_SESSION", "owned"),
            ("HC_SOCKET", "owned-socket"),
            ("HC_TERMINAL_ID", "t1"),
        ]);
        let scoped = environment(&a[1..], &inherited).unwrap();
        assert_eq!(scoped.get("PATH"), Some("client path;literal"));
        assert_eq!(scoped.get("MUX_AGENTS_STATE"), w.ctx.state_root.to_str());
        for k in ["HC_SESSION", "HC_SOCKET", "HC_TERMINAL_ID"] {
            assert_eq!(scoped.get(k), inherited.get(k));
        }
    }

    #[test]
    fn only_exact_six_argument_form_is_scoped() {
        let args = [
            "launcher",
            "spec.json",
            "--state-root",
            "root",
            "--path",
            "path",
        ]
        .map(str::to_owned);
        assert!(is_scoped(&args));
        assert!(!is_scoped(&args[..2]));
        assert!(!is_scoped(&[]));
        for (index, wrong) in [(0, "Launcher"), (2, "--root"), (4, "--PATH")] {
            let mut malformed = args.clone();
            malformed[index] = wrong.into();
            assert!(!is_scoped(&malformed));
        }
    }

    #[test]
    fn scope_grammar_and_root_are_strict() {
        let old = vec!["launcher".into(), "spec.json".into()];
        assert_eq!(spec(&old), Some("spec.json"));
        let env = Env::from_pairs([("PATH", "old")]);
        assert_eq!(environment(&old, &env).unwrap().get("PATH"), Some("old"));
        let mut a: Vec<String> = [
            "launcher",
            "spec.json",
            "--state-root",
            "relative",
            "--path",
            "x",
        ]
        .map(str::to_owned)
        .into();
        assert!(environment(&a, &env).is_err());
        a.push("extra".into());
        assert_eq!(spec(&a), None);
        assert!(environment(&a, &env).is_err());
    }
}
