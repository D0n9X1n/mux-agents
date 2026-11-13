//! Launch artifacts accompany a failed live view read.

#[path = "support/launch_evidence.rs"]
mod launch_evidence;

#[test]
fn failure_contains_launch_artifacts() {
    use std::{fs, path::PathBuf};
    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let t = Temp(std::env::temp_dir().join(format!(
        "mux-evidence-{}",
        mux_agent::util::hex_id().unwrap()
    )));
    let folder = t.0.join("agent");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("launcher.json"), "launcher-exit-1").unwrap();
    fs::write(folder.join("abort.json"), "abort-reason").unwrap();
    fs::write(t.0.join("client.12.json"), "client-session").unwrap();
    fs::write(t.0.join("exited.12"), "7").unwrap();
    let err = launch_evidence::checked(&folder, &t.0, || panic!("missing-view")).unwrap_err();
    for token in [
        "missing-view",
        "launcher.json",
        "launcher-exit-1",
        "abort.json",
        "abort-reason",
        "client.12.json",
        "client-session",
        "exited.12",
        "7",
    ] {
        assert!(err.contains(token), "missing {token}: {err}");
    }
    for file in [
        folder.join("launcher.json"),
        folder.join("abort.json"),
        t.0.join("client.12.json"),
        t.0.join("exited.12"),
    ] {
        assert!(
            err.contains(&format!("\n{}\n", file.display())),
            "missing full path {}: {err}",
            file.display()
        );
    }
    fs::remove_file(folder.join("abort.json")).unwrap();
    let err = launch_evidence::checked(&folder, &t.0, || panic!("gone")).unwrap_err();
    assert!(err.contains("unavailable"));
    assert_eq!(launch_evidence::checked(&folder, &t.0, || 23).unwrap(), 23);
}

#[test]
fn bar_geometry_reads_include_agent_evidence() {
    let source = include_str!("live.rs");
    let start = source
        .find("    fn bar_opens_at_right_edge_from_any_pane(")
        .unwrap();
    let body = &source[start..source[start..].find("    fn damage(").unwrap() + start];
    assert!(
        body.contains("launch_evidence::checked("),
        "bar geometry bypasses launch evidence"
    );
    assert!(body.contains("live.agent_dir(s(&spawned[\"id\"]))"));
    assert!(body.contains("live.dir.path()"));
}
