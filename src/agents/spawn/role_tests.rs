//! `spawn -Role` by a person outside a team, through `spawn` itself.
use super::tests::{agent_folders, agents_dir, calls, empty_launch, human, read_json, reply, run};
use super::*;
use crate::agents::{fixture::World, gens};

/// A person in `<temp>/work` (a repository root), its home at `<temp>/home`, with a task file.
fn world() -> World {
    let mut w = human();
    w.ctx.home = w.ctx.cwd.parent().unwrap().join("home");
    std::fs::create_dir_all(w.ctx.cwd.join(".git")).unwrap();
    w.file("t.md", b"task");
    w
}

fn put(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn claude_role(w: &World, file: &str) -> PathBuf {
    w.ctx.cwd.join(".claude").join("agents").join(file)
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

/// The new member's folder and its generation-0 spec.
fn launched(w: &World, ok: &Value) -> (PathBuf, Value) {
    let folder = agents_dir(w).join(ok["id"].as_str().unwrap());
    let spec = read_json(&gens::path(&folder, 0, "spec"));
    (folder, spec)
}

const ROLE: &str = "---\nname: x\ndescription: reviews\nmodel: haiku\ntools: Read, Grep\npermissionMode: bypassPermissions\nhooks:\n  Stop:\n    - command: x\nmcpServers:\n  - fs\n---\nYou review code.\n";

#[test]
fn a_role_binds_its_model_and_tools_and_its_body_is_the_standing_brief() {
    let w = world();
    put(&claude_role(&w, "x.md"), ROLE);
    empty_launch(&w);
    let ok = reply(&run(
        &w,
        &["-Client", "claude", "-TaskFile", "t.md", "-Role", "x"],
    ));
    assert_eq!((&ok["role"], &ok["model"]), (&json!("x"), &json!("haiku")));
    assert_eq!(
        ok["roleIgnored"],
        json!(["permissionMode", "hooks", "mcpServers"])
    );
    let (folder, spec) = launched(&w, &ok);
    let flags = strings(&spec["flags"]);
    let mut want = launch_flags(Client::Claude, None, None);
    want.extend(["--model=haiku".to_string(), "--tools=Read,Grep".to_string()]);
    assert_eq!(
        flags, want,
        "the person's flags, then the role's two tokens"
    );
    let args = strings(&spec["args"]);
    assert!(
        !flags.iter().chain(&args).any(|a| a.starts_with("--agent")),
        "never --agent: {flags:?} {args:?}"
    );
    assert_eq!(
        std::fs::read_to_string(folder.join("role.md")).unwrap(),
        "You review code.\n"
    );
    let prompt = spec["prompt"].as_str().unwrap();
    let path = folder.join("role.md").to_str().unwrap().replace('\\', "/");
    let line = prompt
        .lines()
        .find(|l| l.starts_with("Role: "))
        .unwrap_or_else(|| panic!("no role line: {prompt}"));
    assert!(line.contains(&path), "{line}");
    assert!(
        prompt.ends_with("\n\ntask"),
        "the task comes after: {prompt}"
    );
    let record = read_json(&folder.join("record.json"));
    assert_eq!(
        (&record["role"], &record["roleClient"], &record["model"]),
        (&json!("x"), &json!("claude"), &json!("haiku"))
    );
    assert_eq!(
        PathBuf::from(record["roleFile"].as_str().unwrap()),
        claude_role(&w, "x.md")
    );
    assert_eq!(record["options"], spec["flags"], "replayed as launched");
}

// -Model wins over the role's model; the role's model is then listed as ignored.
#[test]
fn a_model_flag_keeps_its_place_and_the_roles_model_is_ignored() {
    let w = world();
    put(&claude_role(&w, "x.md"), ROLE);
    empty_launch(&w);
    let ok = reply(&run(
        &w,
        &[
            "-Client",
            "claude",
            "-TaskFile",
            "t.md",
            "-Role",
            "x",
            "-Model",
            "opus",
        ],
    ));
    assert_eq!(ok["model"], "opus");
    assert_eq!(
        ok["roleIgnored"],
        json!(["permissionMode", "hooks", "mcpServers", "model"])
    );
    let (_, spec) = launched(&w, &ok);
    let mut want = launch_flags(Client::Claude, None, None);
    want.push("--tools=Read,Grep".into());
    assert_eq!(strings(&spec["flags"]), want);
    let args = strings(&spec["args"]);
    assert_eq!(&args[2..4], ["--model", "opus"], "{args:?}");
}

/// (role files in the project's `.claude/agents`, the `-Role` value, the start of the reply)
type Case<'a> = (Vec<(&'a str, String)>, &'a str, &'a str);

#[test]
fn a_role_that_cannot_be_used_refuses_before_anything_is_created() {
    let many: Vec<String> = (0..65).map(|i| format!("T{i}")).collect();
    let many = many.join(",");
    let mut cases: Vec<Case> = [
        "--allow-all",
        "--yolo",
        "--dangerously-skip-permissions",
        "-p",
        "Read --allow-all",
        "Read,--yolo",
        "a=b",
        many.as_str(),
    ]
    .iter()
    .map(|tools| {
        (
            vec![("x.md", format!("---\nname: x\ntools: {tools}\n---\nbody\n"))],
            "x",
            "2 role",
        )
    })
    .collect();
    let named = |body: &str| format!("---\nname: x\n---\n{body}\n");
    cases.push((
        vec![("a.md", named("a")), ("b.md", named("b"))],
        "x",
        "2 role",
    ));
    cases.push((vec![], "x", "2 role"));
    cases.push((vec![("x.md", named("x"))], "a b", "2 usage"));
    cases.push((vec![("x.md", named("x"))], "a:b:c", "2 usage"));
    for (files, name, want) in cases {
        let w = world();
        for (file, text) in &files {
            put(&claude_role(&w, file), text);
        }
        let out = run(
            &w,
            &["-Client", "claude", "-TaskFile", "t.md", "-Role", name],
        );
        assert!(out.starts_with(want), "{name} {files:?}: {out}");
        assert!(w.fakes.hc_calls().is_empty(), "{out}: no hc call");
        assert!(agent_folders(&w).is_empty(), "{out}: no agent folder");
        assert!(
            calls(&w, "new-window").is_empty() && calls(&w, "split-window").is_empty(),
            "{out}: no pane"
        );
    }
}

#[test]
fn a_copilot_role_binds_its_tool_list_as_available_tools() {
    let w = world();
    let file = w.ctx.cwd.join(".github").join("agents").join("x.agent.md");
    put(&file, "---\ntools: [read, shell(git)]\n---\nYou test.\n");
    empty_launch(&w);
    let ok = reply(&run(
        &w,
        &["-Client", "copilot", "-TaskFile", "t.md", "-Role", "x"],
    ));
    assert_eq!(
        (&ok["role"], &ok["model"]),
        (&json!("x"), &json!("inherited"))
    );
    let (folder, spec) = launched(&w, &ok);
    let flags = strings(&spec["flags"]);
    let mut want = launch_flags(Client::Copilot, None, None);
    want.push("--available-tools=read,shell(git)".into());
    assert_eq!(flags, want);
    // The grammar reads the whole argv, and replay keeps the token.
    let argv: Vec<String> = flags
        .iter()
        .cloned()
        .chain(strings(&spec["args"]))
        .collect();
    let got = crate::agents::options::classify(Client::Copilot, &argv).unwrap();
    assert_eq!(got.options, flags);
    let ceiling = crate::agents::team::Policy::AllowAll;
    assert_eq!(
        crate::agents::options::replay(Client::Copilot, &flags, ceiling).ok(),
        Some(flags.clone())
    );
    let record = read_json(&folder.join("record.json"));
    assert_eq!(
        (&record["role"], &record["roleClient"]),
        (&json!("x"), &json!("copilot"))
    );
    assert_eq!(PathBuf::from(record["roleFile"].as_str().unwrap()), file);
    assert_eq!(
        std::fs::read_to_string(folder.join("role.md")).unwrap(),
        "You test.\n"
    );
}
