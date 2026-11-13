#![cfg(feature = "live")]
#[path = "support/private_core.rs"]
mod private_core;
use private_core::PrivateCore;
fn root() -> std::path::PathBuf {
    let base = if cfg!(unix) {
        std::path::PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    base.join(format!("mxc-{}", mux_agent::util::hex_id().unwrap()))
}
#[test]
fn core_environment_is_private() {
    let d = root();
    let a = PrivateCore::new("fake".into(), &d.join("a"), "private-path".as_ref());
    let b = PrivateCore::new("fake".into(), &d.join("b"), "private-path".as_ref());
    let mut c = a.command(&[]);
    c.env("HC_TERMINAL_ID", "ambient")
        .env("HC_SOCKET", "ambient")
        .env("TMUX", "ambient");
    a.apply(&mut c);
    let vars: std::collections::BTreeMap<_, _> = c.get_envs().collect();
    assert_eq!(
        vars[std::ffi::OsStr::new("HOME")],
        Some(d.join("a").join("home").as_os_str())
    );
    assert!(!vars.contains_key(std::ffi::OsStr::new("HC_TERMINAL_ID")));
    assert!(!vars.contains_key(std::ffi::OsStr::new("TMUX")));
    assert_eq!(
        vars[std::ffi::OsStr::new("HC_SOCKET")],
        a.env
            .iter()
            .find(|(k, _)| *k == "HC_SOCKET")
            .map(|(_, v)| v.as_os_str())
    );
    assert_ne!(
        a.env.iter().find(|(k, _)| *k == "HC_SESSION"),
        b.env.iter().find(|(k, _)| *k == "HC_SESSION")
    );
    assert_ne!(
        a.env.iter().find(|(k, _)| *k == "HC_SOCKET"),
        b.env.iter().find(|(k, _)| *k == "HC_SOCKET")
    );
    drop((a, b));
    std::fs::remove_dir_all(d).unwrap();
}
#[test]
fn core_environment_keys_are_allowlisted() {
    use std::collections::BTreeSet;

    let d = root();
    let core = PrivateCore::new("fake".into(), &d, "private-path".as_ref());
    let keys: BTreeSet<_> = core.env.iter().map(|(key, _)| *key).collect();
    let expected: BTreeSet<_> = [
        "USER",
        "USERNAME",
        "LOGNAME",
        "LANG",
        "SystemRoot",
        "ComSpec",
        "RMUX_ALLOW_INTERNAL_DAEMON_IN_CALLER_JOB",
    ]
    .into_iter()
    .filter(|key| std::env::var_os(key).is_some())
    .chain([
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_RUNTIME_DIR",
        "TMPDIR",
        "TEMP",
        "TMP",
        "PATH",
        "HC_SESSION",
        "HC_SOCKET",
        "MUX_IT_DIR",
        "MUX_IT_ENTRY",
    ])
    .collect();
    drop(core);
    std::fs::remove_dir_all(d).unwrap();
    assert_eq!(keys, expected);
}

#[test]
fn core_drop_spares_another_fixture() {
    let d = root();
    let bin = PrivateCore::installed();
    let path = std::env::var_os("PATH").unwrap();
    let mut sentinel = PrivateCore::new(bin.clone(), &d.join("s"), &path);
    sentinel.start();
    let mut victim = PrivateCore::new(bin, &d.join("v"), &path);
    victim.start();
    let probe = victim.command(&["server", "stop", "--json"]);
    assert!(
        std::panic::catch_unwind(move || {
            let _guard = victim;
            panic!("fixture failure");
        })
        .is_err()
    );
    assert_eq!(
        private_core::run(probe, &d.join("v")).unwrap()["stopped"],
        false
    );
    assert_eq!(
        private_core::run(
            sentinel.command(&["server", "stop", "--json"]),
            &d.join("s")
        )
        .unwrap()["stopped"],
        true
    );
    assert!(sentinel.stop());
    drop(sentinel);
    std::fs::remove_dir_all(d).unwrap();
}
