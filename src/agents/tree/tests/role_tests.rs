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
            "---\nname: x\nmodel: haiku\ntools: Read, Grep\npermissionMode: bypassPermissions\nhooks:\n  - audit\nmcpServers:\n  - fs\n---\n{body}\n"
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
