//! `spawn -Role` by a proven member: its child starts where the member started, and the role is found there.
use super::*;

fn put(path: &std::path::Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A member recorded in `<temp>/w` calls from `<temp>/w/sub`; `x` is in both, with permission fields that a default
/// team never grants. The child takes `/w`'s, and its permission flags are a role-less child's.
#[test]
fn a_members_child_takes_its_role_from_where_the_member_started() {
    let mut m = member_world();
    let temp = m.w.ctx.cwd.parent().unwrap().to_path_buf();
    let (root, sub) = (temp.join("w"), temp.join("w").join("sub"));
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let role = |body: &str| {
        format!(
            "---\nname: x\nmodel: haiku\ntools: Read, Grep\npermissionMode: bypassPermissions\nhooks:\n  PreToolUse:\n    - matcher: Bash\n      hooks:\n        - type: command\n          command: ./check.sh\nmcpServers:\n  github:\n    command: npx\n    args: [\"-y\", \"@x/server\"]\n---\n{body}\n"
        )
    };
    put(
        &root.join(".claude").join("agents").join("x.md"),
        &role("from w"),
    );
    put(
        &sub.join(".claude").join("agents").join("x.md"),
        &role("from sub"),
    );
    let file = m.folder(ID).join("record.json");
    let mut r = crate::state::fs::read_json(&file).unwrap().unwrap();
    r["directory"] = json!(root);
    write(&file, r.clone());
    m.r = r;
    m.w.ctx.home = temp.join("home");
    m.w.ctx.cwd = sub;
    std::fs::create_dir_all(&m.w.ctx.cwd).unwrap();
    m.w.file("t.md", b"task");
    m.w.file("b.md", b"brief");
    m.hc();
    spawn_rounds(&m, &term(&m.r, Some(150)));
    launch_ok(&m.w);
    let args = [
        "spawn",
        "-Client",
        "claude",
        "-TaskFile",
        "t.md",
        "-BriefFile",
        "b.md",
        "-Context",
        "brief",
        "-Name",
        "child",
        "-Role",
        "x",
    ]
    .map(String::from);
    let (code, out) = crate::cli::run(&args, || Ok(&m.w.ctx));
    assert_eq!(code, 0, "{out}");
    let ok: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(
        ok["roleIgnored"],
        json!(["permissionMode", "hooks", "mcpServers"])
    );
    let child = m.folder(ok["id"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(child.join("role.md")).unwrap(),
        "from w\n"
    );
    let spec = crate::state::fs::read_json(&crate::agents::gens::path(&child, 0, "spec"))
        .unwrap()
        .unwrap();
    let flags: Vec<String> = spec["flags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap().to_string())
        .collect();
    let plain = child_flags(Client::Claude, t::Policy::Default, t::Policy::Default, None).unwrap();
    let mut want = plain.clone();
    want.extend(["--model=haiku".to_string(), "--tools=Read,Grep".to_string()]);
    assert_eq!(flags, want);
    assert_eq!(
        t::policy(Client::Claude, &flags),
        t::policy(Client::Claude, &plain)
    );
    m.w.fakes.hc_done().unwrap();
}

/// A member's spawn of a role it cannot use refuses 2 `role` from local files alone: no hc call is made first.
#[test]
fn a_members_missing_role_refuses_before_any_hc_call() {
    let mut m = member_world();
    m.w.ctx.home = m.w.ctx.cwd.parent().unwrap().join("home");
    m.w.file("t.md", b"task");
    m.w.file("b.md", b"brief");
    m.hc();
    let before = m.w.fakes.hc_calls().len();
    let args = [
        "spawn",
        "-Client",
        "claude",
        "-TaskFile",
        "t.md",
        "-BriefFile",
        "b.md",
        "-Context",
        "brief",
        "-Role",
        "nope",
    ]
    .map(String::from);
    let (code, out) = crate::cli::run(&args, || Ok(&m.w.ctx));
    assert_eq!(code, 2, "{out}");
    assert!(out.contains(r#""check":"role""#), "{out}");
    assert_eq!(m.w.fakes.hc_calls().len(), before, "no hc call: {out}");
}

/// A member recorded in `<temp>/w` calling from `<temp>/w/sub`, with role `x` in `/w` only; the role file's path.
fn rooted(m: &mut Mw) -> std::path::PathBuf {
    let temp = m.w.ctx.cwd.parent().unwrap().to_path_buf();
    let (root, sub) = (temp.join("w"), temp.join("w").join("sub"));
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(&sub).unwrap();
    let role = root.join(".claude").join("agents").join("x.md");
    put(&role, "---\nname: x\ntools: Read\n---\nfrom w\n");
    let file = m.folder(ID).join("record.json");
    let mut r = crate::state::fs::read_json(&file).unwrap().unwrap();
    r["directory"] = json!(root);
    write(&file, r.clone());
    m.r = r;
    m.w.ctx.home = temp.join("home");
    m.w.ctx.cwd = sub;
    m.w.file("t.md", b"task");
    m.w.file("b.md", b"brief");
    role
}

fn spawn_x() -> [String; 13] {
    [
        "spawn",
        "-Client",
        "claude",
        "-TaskFile",
        "t.md",
        "-BriefFile",
        "b.md",
        "-Context",
        "brief",
        "-Name",
        "child",
        "-Role",
        "x",
    ]
    .map(String::from)
}

/// The dispatcher's check reads the role once, before any hc call, and spawn uses that snapshot: a role file deleted
/// on the first hc call still spawns from it, and the definition is read exactly once per spawn.
#[test]
fn a_members_spawn_uses_the_snapshot_read_before_the_proof() {
    let mut m = member_world();
    let role = rooted(&mut m);
    m.hc();
    spawn_rounds(&m, &term(&m.r, Some(150)));
    launch_ok(&m.w);
    let gone = role.clone();
    m.w.fakes.hc_probe(move |_| {
        let _ = std::fs::remove_file(&gone);
    });
    let ((code, out), reads) =
        crate::agents::role::record_reads(|| crate::cli::run(&spawn_x(), || Ok(&m.w.ctx)));
    assert_eq!(code, 0, "{out}");
    assert_eq!(reads, vec![role.clone()], "the role file is read once");
    let ok: Value = serde_json::from_str(out.trim()).unwrap();
    let child = m.folder(ok["id"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(child.join("role.md")).unwrap(),
        "from w\n"
    );
    m.w.fakes.hc_done().unwrap();
}

/// Once hc proves the member, the launch folder it gives must be the one the snapshot was read from: a record whose
/// folder changed in between refuses 2 `role` before anything is allocated.
#[test]
fn a_snapshot_from_another_folder_than_the_proven_members_refuses_before_allocate() {
    let mut m = member_world();
    rooted(&mut m);
    m.hc();
    spawn_rounds(&m, &term(&m.r, Some(150)));
    launch_ok(&m.w);
    let (file, sub) = (m.folder(ID).join("record.json"), m.w.ctx.cwd.clone());
    let moved = std::cell::Cell::new(false);
    m.w.fakes.hc_probe(move |_| {
        if !moved.replace(true) {
            let mut r = crate::state::fs::read_json(&file).unwrap().unwrap();
            r["directory"] = json!(sub);
            write(&file, r);
        }
    });
    let before: Vec<_> = std::fs::read_dir(m.state.join("agents")).unwrap().collect();
    let (code, out) = crate::cli::run(&spawn_x(), || Ok(&m.w.ctx));
    assert_eq!(code, 2, "{out}");
    assert!(out.contains(r#""check":"role""#), "{out}");
    let after: Vec<_> = std::fs::read_dir(m.state.join("agents")).unwrap().collect();
    assert_eq!(after.len(), before.len(), "nothing allocated: {out}");
    assert!(
        !m.w.fakes
            .hc_calls()
            .iter()
            .any(|(_, a, _)| a.first().is_some_and(|a| a == "run")),
        "no launch"
    );
}
