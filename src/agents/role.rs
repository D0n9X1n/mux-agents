//! `spawn -Role <name>`: a role is the spawning client's own agent definition, read once at spawn. Its body becomes
//! the member's standing brief (`role.md`); only its `model` and `tools` reach the client, each as one bound token;
//! every other field is listed as ignored. A role never widens what the member may do, and the client's own
//! `--agent` is never passed.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{Res, refuse};
use crate::parse::Client;

/// What spawn takes from a role file, read once before any pane, record or hc terminal exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub name: String,
    pub body: String,
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    pub ignored: Vec<String>,
    pub path: PathBuf,
    /// The launch folder the role was resolved from: the launch must start there.
    pub folder: PathBuf,
    pub client: Client,
}

/// A snapshot as one launch uses it: the bound tokens, the model they choose, and every field left unused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    pub snapshot: Snapshot,
    pub tokens: Vec<String>,
    pub model: Option<String>,
    pub ignored: Vec<String>,
}

const NAME_MAX: usize = 64;
const FILE_MAX: u64 = 64 << 10;
const TOOLS_MAX: usize = 64;

/// `-Role`: 1-64 ASCII letters, digits, `-`, `_` and `.`, with at most one `:` between two nonempty parts (a
/// plugin's agent). The value is only ever compared with names and file ids, never joined into a path with a
/// separator in it.
pub fn valid_name(name: &str) -> bool {
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    };
    name.len() <= NAME_MAX
        && match name.split_once(':') {
            Some((plugin, agent)) => plain(plugin) && plain(agent),
            None => plain(name),
        }
}

/// The role `name` of `client`, resolved from `folder` (the folder the child starts in) and `home`, read once.
/// Claude: `.claude/agents/**/*.md` from the folder up to the repository root (closest first), then
/// `~/.claude/agents/**/*.md`, then each installed plugin's `agents/*.md`, matched on the frontmatter `name`.
/// Copilot, personal first as it documents: `~/.copilot/agents/`, then `.github/agents/` from the folder up to the
/// root, then each installed plugin's `agents/`, matched on the file id (`<id>.agent.md`, else `<id>.md`).
/// `plugin:name` searches that plugin alone. The first level with a match wins; two at one level, or none, refuse 2
/// `role`. No other client's folders are read.
pub fn resolve(home: &Path, folder: &Path, client: Client, name: &str) -> Res<Snapshot> {
    let (plugin, bare) = match name.split_once(':') {
        Some((plugin, agent)) => (Some(plugin), agent),
        None => (None, name),
    };
    let levels = match client {
        Client::Claude => claude_levels(home, folder, plugin),
        Client::Copilot => copilot_levels(home, folder, plugin),
    };
    let mut searched = Vec::new();
    for level in &levels {
        let mut found: Vec<(PathBuf, Vec<u8>)> = Vec::new();
        for dir in &level.folders {
            searched.push(show(dir));
            match client {
                Client::Claude => {
                    for path in markdown(dir, level.deep) {
                        if let Some(bytes) = read_head(&path)
                            && declared_name(&bytes).as_deref() == Some(bare)
                        {
                            found.push((path, bytes));
                        }
                    }
                }
                Client::Copilot => {
                    if let Some(path) = by_id(dir, bare) {
                        let Some(bytes) = read_head(&path) else {
                            return refuse(
                                2,
                                "role",
                                format!("the role file {} cannot be read", show(&path)),
                            );
                        };
                        found.push((path, bytes));
                    }
                }
            }
        }
        match found.len() {
            0 => continue,
            1 => {
                let (path, bytes) = found.remove(0);
                return load(&path, bytes, bare, folder, client);
            }
            _ => {
                let files: Vec<String> = found.iter().map(|(p, _)| show(p)).collect();
                return refuse(
                    2,
                    "role",
                    format!("role {name} is ambiguous: {}", files.join(", ")),
                );
            }
        }
    }
    let searched = if searched.is_empty() {
        "no folder".to_string()
    } else {
        searched.join(", ")
    };
    let client = crate::agents::spawn::client_name(client);
    refuse(
        2,
        "role",
        format!("no {client} role named {name}; searched {searched}"),
    )
}

impl Snapshot {
    /// The tokens one launch binds: `--model=<m>` when `-Model` is absent and the role's model passes the model check
    /// (`inherit`, the client's own default, binds none), then the client's one tool-list token. A model not used is
    /// added to `ignored`.
    pub fn bind(&self, model_flag: bool) -> Bound {
        let mut ignored = self.ignored.clone();
        let wanted = self.model.as_deref().filter(|m| *m != "inherit");
        let model = wanted.filter(|m| !model_flag && crate::agents::spawn::model_name(m));
        if wanted.is_some() && model.is_none() {
            ignored.push("model".into());
        }
        let mut tokens: Vec<String> = model.map(|m| format!("--model={m}")).into_iter().collect();
        if let Some(tools) = &self.tools {
            let flag = match self.client {
                Client::Claude => "--tools",
                Client::Copilot => "--available-tools",
            };
            tokens.push(format!("{flag}={}", tools.join(",")));
        }
        Bound {
            snapshot: self.clone(),
            tokens,
            model: model.map(String::from),
            ignored,
        }
    }
}

/// Admission of a role over the complete argv a launch emits (`flags`, then `tokens`, then `args`): the team grammar
/// reads it whole, and it gives the policy the same argv gives without the role's tokens, so a role never changes
/// what the member may do.
pub fn admit(client: Client, flags: &[String], tokens: &[String], args: &[String]) -> Res<()> {
    use super::team;
    let plain: Vec<String> = flags.iter().chain(args).cloned().collect();
    let full: Vec<String> = flags.iter().chain(tokens).chain(args).cloned().collect();
    let same = team::args(client, &full).is_ok()
        && team::policy(client, &full) == team::policy(client, &plain);
    if !same {
        return refuse(
            2,
            "role",
            "the role's tokens would change what the member may do",
        );
    }
    Ok(())
}

/// One folder group searched together: two matches in it are ambiguous.
struct Level {
    folders: Vec<PathBuf>,
    /// Claude reads `.claude/agents` and every folder below it; a plugin's `agents/` is read flat.
    deep: bool,
}

fn claude_levels(home: &Path, folder: &Path, plugin: Option<&str>) -> Vec<Level> {
    let mut levels = Vec::new();
    if plugin.is_none() {
        for dir in project(folder) {
            levels.push(Level {
                folders: vec![dir.join(".claude").join("agents")],
                deep: true,
            });
        }
        levels.push(Level {
            folders: vec![home.join(".claude").join("agents")],
            deep: true,
        });
    }
    let plugins = claude_plugins(home, plugin);
    if !plugins.is_empty() {
        levels.push(Level {
            folders: plugins,
            deep: false,
        });
    }
    levels
}

/// Each installed plugin's `agents/`, from `~/.claude/plugins/installed_plugins.json` (`plugins` maps
/// `<plugin>@<marketplace>` to its installs, each with an absolute `installPath`); none when the file is missing or
/// unreadable. Only `plugin`'s, when one is named.
fn claude_plugins(home: &Path, plugin: Option<&str>) -> Vec<PathBuf> {
    let file = home
        .join(".claude")
        .join("plugins")
        .join("installed_plugins.json");
    let Some(listed) = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = Vec::new();
    let plugins = listed.get("plugins").and_then(Value::as_object);
    for (key, installs) in plugins.into_iter().flatten() {
        let name = key.split_once('@').map_or(key.as_str(), |(n, _)| n);
        if plugin.is_some_and(|p| p != name) {
            continue;
        }
        let installs: Vec<&Value> = match installs {
            Value::Array(all) => all.iter().collect(),
            one => vec![one],
        };
        for install in installs {
            let Some(path) = install
                .get("installPath")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
            else {
                continue;
            };
            let agents = path.join("agents");
            if !out.contains(&agents) {
                out.push(agents);
            }
        }
    }
    out
}

fn copilot_levels(home: &Path, folder: &Path, plugin: Option<&str>) -> Vec<Level> {
    let mut levels = Vec::new();
    if plugin.is_none() {
        levels.push(Level {
            folders: vec![home.join(".copilot").join("agents")],
            deep: false,
        });
        for dir in project(folder) {
            levels.push(Level {
                folders: vec![dir.join(".github").join("agents")],
                deep: false,
            });
        }
    }
    // `~/.copilot/installed-plugins/<marketplace>/<plugin>/agents/`.
    let mut plugins = Vec::new();
    for market in folders(&home.join(".copilot").join("installed-plugins")) {
        for dir in folders(&market) {
            if plugin.is_none_or(|p| dir.file_name().is_some_and(|n| n == p)) {
                plugins.push(dir.join("agents"));
            }
        }
    }
    if !plugins.is_empty() {
        levels.push(Level {
            folders: plugins,
            deep: false,
        });
    }
    levels
}

/// The launch folder and each parent up to the repository root (the first holding `.git`), closest first; with no
/// repository root, the launch folder alone.
fn project(folder: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in folder.ancestors() {
        out.push(dir.to_path_buf());
        if dir.join(".git").exists() {
            return out;
        }
    }
    vec![folder.to_path_buf()]
}

/// The folders in `dir`, sorted.
fn folders(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

/// The `*.md` regular files in `dir`, sorted, and with `deep` those of every folder below it. A linked folder is never
/// entered, so the walk ends.
fn markdown(dir: &Path, deep: bool) -> Vec<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|e| e.file_name());
    let mut out = Vec::new();
    for e in entries {
        let path = e.path();
        match e.file_type() {
            Ok(kind) if kind.is_dir() => {
                if deep {
                    out.extend(markdown(&path, true));
                }
            }
            Ok(_) if path.extension().is_some_and(|x| x == "md") && path.is_file() => {
                out.push(path)
            }
            _ => {}
        }
    }
    out
}

/// Copilot's file for `id` in `dir`: `<id>.agent.md`, else `<id>.md` (never for an id ending `.agent`, whose `.md`
/// file is another id's `.agent.md`).
fn by_id(dir: &Path, id: &str) -> Option<PathBuf> {
    let agent = dir.join(format!("{id}.agent.md"));
    if agent.is_file() {
        return Some(agent);
    }
    let plain = dir.join(format!("{id}.md"));
    (!id.ends_with(".agent") && plain.is_file()).then_some(plain)
}

/// A regular file's first bytes, at most one past the cap; None when it cannot be read.
fn read_head(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    if !path.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(FILE_MAX + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

/// The frontmatter `name` of a Claude role file, read from its head.
fn declared_name(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let (lines, _) = split(&text)?;
    fields(&lines)?
        .into_iter()
        .find_map(|(key, value)| match (key.as_str(), value) {
            ("name", Field::Text(name)) => Some(name),
            _ => None,
        })
}

/// The matched file as spawn uses it: whole (at most 64 KiB), UTF-8, its frontmatter readable.
fn load(path: &Path, bytes: Vec<u8>, name: &str, folder: &Path, client: Client) -> Res<Snapshot> {
    let shown = show(path);
    if bytes.len() as u64 > FILE_MAX {
        return refuse(
            2,
            "role",
            format!("the role file {shown} is larger than {FILE_MAX} bytes"),
        );
    }
    let (Ok(text), Some(_)) = (String::from_utf8(bytes), path.to_str()) else {
        return refuse(2, "role", format!("the role file {shown} is not UTF-8"));
    };
    let malformed = || {
        refuse(
            2,
            "role",
            format!("the role file {shown} has a frontmatter this cannot read"),
        )
    };
    let Some((lines, body)) = split(&text) else {
        return malformed();
    };
    let Some(fields) = fields(&lines) else {
        return malformed();
    };
    let (mut model, mut tools, mut ignored) = (None, None, Vec::new());
    for (key, value) in fields {
        match (key.as_str(), value) {
            // What names and describes the role, not what the member does.
            ("name" | "description", _) | ("model" | "tools", Field::Empty) => {}
            ("model", Field::Text(m)) => model = Some(m),
            ("tools", value) => {
                let Some(names) = tool_list(value) else {
                    return refuse(
                        2,
                        "role",
                        format!(
                            "the role file {shown} names a tool this never passes on: at most {TOOLS_MAX} names, \
                             each a letter then letters, digits and _ * ( ) . :"
                        ),
                    );
                };
                tools = Some(names);
            }
            _ => ignored.push(key),
        }
    }
    Ok(Snapshot {
        name: name.into(),
        body: body.into(),
        model,
        tools,
        ignored,
        path: path.into(),
        folder: folder.into(),
        client,
    })
}

/// `tools` as a comma-separated string or a list: 1-64 names, each one tool and never a flag or a second argument.
fn tool_list(value: Field) -> Option<Vec<String>> {
    let names: Vec<String> = match value {
        Field::Text(text) => text.split(',').map(|n| n.trim().to_string()).collect(),
        Field::List(list) => list,
        _ => return None,
    };
    let ok = !names.is_empty() && names.len() <= TOOLS_MAX && names.iter().all(|n| tool_name(n));
    ok.then_some(names)
}

/// `^[A-Za-z][A-Za-z0-9_*().:]*$`: no leading `-`, no space, `,` or `=`.
fn tool_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || b"_*().:".contains(&b))
}

/// A frontmatter value, as far as this minimal reader goes: nothing, text, a list of text, or anything else (a map, a
/// block, a list of maps), which nothing here uses.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Field {
    Empty,
    Text(String),
    List(Vec<String>),
    Other,
}

/// A `---` frontmatter's lines and the body after its closing `---`; a file that does not open with `---` is all
/// body. None for a frontmatter that never closes. CRLF reads like LF.
fn split(text: &str) -> Option<(Vec<&str>, &str)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let line_end = |s: &str| s.find('\n').map_or(s.len(), |i| i + 1);
    let first = &text[..line_end(text)];
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return Some((Vec::new(), text));
    }
    let mut at = first.len();
    let mut lines = Vec::new();
    while at < text.len() {
        let end = at + line_end(&text[at..]);
        let line = text[at..end].trim_end_matches(['\r', '\n']);
        if line == "---" {
            return Some((lines, &text[end..]));
        }
        lines.push(line);
        at = end;
    }
    None
}

/// Top-level `key: value` lines, each with its indented or `- ` lines below it: a value is text, a `[a, b]` or `- a`
/// list of text, or `Other`. A later key replaces an earlier one. None when the first line holds no key.
fn fields(lines: &[&str]) -> Option<Vec<(String, Field)>> {
    let mut out: Vec<(String, Field)> = Vec::new();
    for line in lines {
        let bare = line.trim();
        if bare.is_empty() || bare.starts_with('#') {
            continue;
        }
        let key_value = line
            .split_once(':')
            .filter(|_| !line.starts_with([' ', '\t', '-']));
        let Some((key, value)) = key_value else {
            // A line under the last key: a `- item` extends its list; anything else makes it a value not used here.
            let (_, last) = out.last_mut()?;
            let item = bare
                .strip_prefix('-')
                .filter(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
                .map(|rest| scalar(rest.trim()));
            *last = match (std::mem::replace(last, Field::Other), item) {
                (Field::Empty, Some(Field::Text(t))) => Field::List(vec![t]),
                (Field::List(mut list), Some(Field::Text(t))) => {
                    list.push(t);
                    Field::List(list)
                }
                _ => Field::Other,
            };
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        let value = scalar(value.trim());
        match out.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v = value,
            None => out.push((key.into(), value)),
        }
    }
    Some(out)
}

/// One value: quotes are taken off; `[a, b]` is a list of such values; a map, block or anchor is `Other`.
fn scalar(value: &str) -> Field {
    if value.is_empty() {
        return Field::Empty;
    }
    if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        if inner.trim().is_empty() {
            return Field::List(Vec::new());
        }
        let items: Option<Vec<String>> = inner
            .split(',')
            .map(|item| match scalar(item.trim()) {
                Field::Text(t) => Some(t),
                Field::Empty => Some(String::new()),
                _ => None,
            })
            .collect();
        return items.map_or(Field::Other, Field::List);
    }
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return Field::Text(value[1..value.len() - 1].into());
        }
    }
    if value.starts_with(['[', '{', '|', '>', '&', '*', '!']) || value.contains(": ") {
        return Field::Other;
    }
    Field::Text(value.into())
}

/// A path as messages show it, with forward slashes.
fn show(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::team::Policy;
    use crate::testing::TempDir;

    /// A temp home and a launch folder `<temp>/w` that is a repository root.
    struct Fx {
        dir: TempDir,
    }

    impl Fx {
        fn new() -> Fx {
            let fx = Fx {
                dir: TempDir::new(),
            };
            std::fs::create_dir_all(fx.home()).unwrap();
            std::fs::create_dir_all(fx.work().join(".git")).unwrap();
            fx
        }
        fn home(&self) -> PathBuf {
            self.dir.path().join("home")
        }
        fn work(&self) -> PathBuf {
            self.dir.path().join("w")
        }
        fn put(&self, path: &Path, text: &str) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        fn get(&self, client: Client, folder: &Path, name: &str) -> Res<Snapshot> {
            resolve(&self.home(), folder, client, name)
        }
    }

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn check<T: std::fmt::Debug>(r: Res<T>) -> (u8, &'static str) {
        let e = r.expect_err("a refusal");
        (e.code, e.check)
    }

    const X: &str = "---\nname: x\nmodel: haiku\ntools: Read, Grep\npermissionMode: bypassPermissions\nhooks:\n  PreToolUse:\n    - matcher: Bash\nmcpServers:\n  - fs\n---\nYou review code.\n";

    #[test]
    fn a_role_name_is_a_plain_token_with_at_most_one_colon() {
        for ok in [
            "x",
            "code-reviewer",
            "a_b.c",
            "plugin:agent",
            &"a".repeat(64),
        ] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            &"a".repeat(65),
            "a:b:c",
            ":x",
            "x:",
            "a b",
            "a/b",
            "a\\b",
            "--yolo=1",
            "é",
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_claude_role_is_found_by_its_frontmatter_name_anywhere_below_agents() {
        let fx = Fx::new();
        fx.put(&fx.work().join(".claude/agents/team/review.md"), X);
        let s = fx.get(Client::Claude, &fx.work(), "x").unwrap();
        assert_eq!(s.name, "x");
        assert_eq!(s.body, "You review code.\n");
        assert_eq!(s.model.as_deref(), Some("haiku"));
        assert_eq!(s.tools, Some(v(&["Read", "Grep"])));
        assert_eq!(s.ignored, v(&["permissionMode", "hooks", "mcpServers"]));
        assert_eq!(s.path, fx.work().join(".claude/agents/team/review.md"));
        assert_eq!(s.folder, fx.work());
        let b = s.bind(false);
        assert_eq!(b.tokens, v(&["--model=haiku", "--tools=Read,Grep"]));
        assert_eq!(b.model.as_deref(), Some("haiku"));
        assert_eq!(b.ignored, s.ignored);
        // -Model wins: the role's model is not used, and says so.
        let b = s.bind(true);
        assert_eq!(b.tokens, v(&["--tools=Read,Grep"]));
        assert_eq!(b.model, None);
        assert_eq!(
            b.ignored,
            v(&["permissionMode", "hooks", "mcpServers", "model"])
        );
    }

    // `inherit` (Claude's own default) and a value the model check refuses choose no model.
    #[test]
    fn a_model_that_is_not_a_model_name_binds_nothing() {
        let fx = Fx::new();
        for (model, ignored) in [("inherit", false), ("--yolo", true), ("a b", true)] {
            fx.put(
                &fx.work().join(".claude/agents/x.md"),
                &format!("---\nname: x\nmodel: \"{model}\"\n---\nbody\n"),
            );
            let b = fx.get(Client::Claude, &fx.work(), "x").unwrap().bind(false);
            assert!(b.tokens.is_empty(), "{model}");
            assert_eq!(b.model, None, "{model}");
            assert_eq!(b.ignored.contains(&"model".to_string()), ignored, "{model}");
        }
    }

    #[test]
    fn a_tool_that_could_be_a_flag_refuses_the_role() {
        let fx = Fx::new();
        let many: Vec<String> = (0..65).map(|i| format!("T{i}")).collect();
        let many = many.join(", ");
        for tools in [
            "--allow-all",
            "--yolo",
            "--dangerously-skip-permissions",
            "-p",
            "Read --allow-all",
            "Read,--yolo",
            "a=b",
            "Read,,Grep",
            "[]",
            "\"\"",
            "github/*",
            "1Read",
            many.as_str(),
        ] {
            fx.put(
                &fx.work().join(".claude/agents/x.md"),
                &format!("---\nname: x\ntools: {tools}\n---\nbody\n"),
            );
            assert_eq!(
                check(fx.get(Client::Claude, &fx.work(), "x")),
                (2, "role"),
                "{tools}"
            );
        }
        // A list item is one name: a comma or a space inside it refuses.
        for item in ["Read, Grep", "Read --yolo"] {
            fx.put(
                &fx.work().join(".claude/agents/x.md"),
                &format!("---\nname: x\ntools:\n  - \"{item}\"\n---\nbody\n"),
            );
            assert_eq!(
                check(fx.get(Client::Claude, &fx.work(), "x")),
                (2, "role"),
                "{item}"
            );
        }
        // A nested map is no tool list.
        fx.put(
            &fx.work().join(".claude/agents/x.md"),
            "---\nname: x\ntools:\n  Read: yes\n---\nbody\n",
        );
        assert_eq!(check(fx.get(Client::Claude, &fx.work(), "x")), (2, "role"));
        // Tool spellings the clients use pass, as a list or a string.
        let names = ["Bash(git:*)", "mcp__fs__read", "shell(git)", "Web*", "a.b"];
        fx.put(
            &fx.work().join(".claude/agents/x.md"),
            &format!(
                "---\nname: x\ntools: [{}]\n---\nbody\n",
                names
                    .iter()
                    .map(|n| format!("\"{n}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        let s = fx.get(Client::Claude, &fx.work(), "x").unwrap();
        assert_eq!(s.tools, Some(v(&names)));
    }

    // The launch folder and each parent up to the repository root, closest first; never above the root.
    #[test]
    fn the_closest_project_folder_wins_and_none_above_the_root_is_read() {
        let fx = Fx::new();
        let sub = fx.work().join("sub");
        let role = |marker: &str| format!("---\nname: x\n---\n{marker}\n");
        fx.put(&fx.work().join(".claude/agents/x.md"), &role("root"));
        fx.put(&sub.join(".claude/agents/x.md"), &role("sub"));
        fx.put(
            &fx.dir.path().join(".claude/agents/y.md"),
            "---\nname: y\n---\nabove\n",
        );
        fx.put(&fx.work().join(".github/agents/x.agent.md"), "root\n");
        fx.put(&sub.join(".github/agents/x.agent.md"), "sub\n");
        assert_eq!(fx.get(Client::Claude, &sub, "x").unwrap().body, "sub\n");
        assert_eq!(
            fx.get(Client::Claude, &fx.work(), "x").unwrap().body,
            "root\n"
        );
        assert_eq!(fx.get(Client::Copilot, &sub, "x").unwrap().body, "sub\n");
        assert_eq!(check(fx.get(Client::Claude, &sub, "y")), (2, "role"));
    }

    #[test]
    fn two_matches_at_one_level_are_ambiguous_and_none_names_the_folders_searched() {
        let fx = Fx::new();
        fx.put(
            &fx.work().join(".claude/agents/a.md"),
            "---\nname: x\n---\na\n",
        );
        fx.put(
            &fx.work().join(".claude/agents/b/c.md"),
            "---\nname: x\n---\nb\n",
        );
        let e = fx.get(Client::Claude, &fx.work(), "x").unwrap_err();
        assert_eq!((e.code, e.check), (2, "role"));
        assert!(e.message.contains("ambiguous"), "{}", e.message);
        let e = fx.get(Client::Claude, &fx.work(), "nope").unwrap_err();
        assert_eq!((e.code, e.check), (2, "role"));
        for folder in [
            fx.work().join(".claude").join("agents"),
            fx.home().join(".claude").join("agents"),
        ] {
            let shown = folder.to_string_lossy().replace('\\', "/");
            assert!(e.message.contains(&shown), "{}: {shown}", e.message);
        }
        let e = fx.get(Client::Copilot, &fx.work(), "nope").unwrap_err();
        assert_eq!((e.code, e.check), (2, "role"));
        assert!(e.message.contains(".github/agents"), "{}", e.message);
        assert!(e.message.contains(".copilot/agents"), "{}", e.message);
    }

    // Claude's project role wins over the personal one; Copilot reads personal first, as each client does.
    #[test]
    fn a_name_in_home_and_project_resolves_as_each_client_orders_them() {
        let fx = Fx::new();
        fx.put(
            &fx.work().join(".claude/agents/x.md"),
            "---\nname: x\n---\nproject\n",
        );
        fx.put(
            &fx.home().join(".claude/agents/x.md"),
            "---\nname: x\n---\npersonal\n",
        );
        fx.put(&fx.work().join(".github/agents/x.agent.md"), "project\n");
        fx.put(&fx.home().join(".copilot/agents/x.agent.md"), "personal\n");
        assert_eq!(
            fx.get(Client::Claude, &fx.work(), "x").unwrap().body,
            "project\n"
        );
        assert_eq!(
            fx.get(Client::Copilot, &fx.work(), "x").unwrap().body,
            "personal\n"
        );
        // The personal level alone, when the project has none.
        std::fs::remove_file(fx.work().join(".claude/agents/x.md")).unwrap();
        assert_eq!(
            fx.get(Client::Claude, &fx.work(), "x").unwrap().body,
            "personal\n"
        );
    }

    #[test]
    fn a_copilot_role_is_its_file_id_and_agent_md_comes_first() {
        let fx = Fx::new();
        let agents = fx.work().join(".github/agents");
        fx.put(
            &agents.join("x.agent.md"),
            "---\nname: other\ntools: [read, shell(git)]\nmodel: gpt-5\n---\nfirst\n",
        );
        fx.put(&agents.join("x.md"), "second\n");
        fx.put(&agents.join("y.md"), "plain\n");
        let s = fx.get(Client::Copilot, &fx.work(), "x").unwrap();
        assert_eq!(s.body, "first\n");
        assert_eq!(s.path, agents.join("x.agent.md"));
        assert_eq!(
            s.bind(false).tokens,
            v(&["--model=gpt-5", "--available-tools=read,shell(git)"])
        );
        assert_eq!(
            fx.get(Client::Copilot, &fx.work(), "y").unwrap().body,
            "plain\n"
        );
        // `x.agent` is no id of `x.agent.md`.
        assert_eq!(
            check(fx.get(Client::Copilot, &fx.work(), "x.agent")),
            (2, "role")
        );
        // Copilot never reads Claude's folders, nor Claude Copilot's.
        assert_eq!(check(fx.get(Client::Claude, &fx.work(), "y")), (2, "role"));
    }

    #[test]
    fn a_plugin_agent_resolves_by_name_and_by_plugin_name() {
        let fx = Fx::new();
        let install = fx.home().join(".claude/plugins/cache/market/kit/1.0.0");
        fx.put(
            &install.join("agents/rev.md"),
            "---\nname: rev\n---\nplugin\n",
        );
        fx.put(
            &install.join("agents/deep/no.md"),
            "---\nname: deep\n---\nno\n",
        );
        let listed = serde_json::json!({"version": 2, "plugins": {"kit@market": [
            {"scope": "user", "installPath": install}]}});
        fx.put(
            &fx.home().join(".claude/plugins/installed_plugins.json"),
            &listed.to_string(),
        );
        assert_eq!(
            fx.get(Client::Claude, &fx.work(), "rev").unwrap().body,
            "plugin\n"
        );
        assert_eq!(
            fx.get(Client::Claude, &fx.work(), "kit:rev").unwrap().body,
            "plugin\n"
        );
        assert_eq!(
            check(fx.get(Client::Claude, &fx.work(), "other:rev")),
            (2, "role")
        );
        assert_eq!(
            check(fx.get(Client::Claude, &fx.work(), "deep")),
            (2, "role")
        );
        // A project role of the same name comes first; `plugin:name` still reaches the plugin.
        fx.put(
            &fx.work().join(".claude/agents/rev.md"),
            "---\nname: rev\n---\nproject\n",
        );
        assert_eq!(
            fx.get(Client::Claude, &fx.work(), "rev").unwrap().body,
            "project\n"
        );
        assert_eq!(
            fx.get(Client::Claude, &fx.work(), "kit:rev").unwrap().body,
            "plugin\n"
        );
        let copilot = fx
            .home()
            .join(".copilot/installed-plugins/market/kit/agents");
        fx.put(&copilot.join("rev.agent.md"), "copilot plugin\n");
        assert_eq!(
            fx.get(Client::Copilot, &fx.work(), "rev").unwrap().body,
            "copilot plugin\n"
        );
        assert_eq!(
            fx.get(Client::Copilot, &fx.work(), "kit:rev").unwrap().body,
            "copilot plugin\n"
        );
        assert_eq!(
            check(fx.get(Client::Copilot, &fx.work(), "kat:rev")),
            (2, "role")
        );
    }

    #[test]
    fn a_role_file_is_read_whole_once_and_capped() {
        let fx = Fx::new();
        let path = fx.work().join(".github/agents/big.md");
        fx.put(&path, &"x".repeat(64 * 1024));
        assert!(fx.get(Client::Copilot, &fx.work(), "big").is_ok());
        fx.put(&path, &"x".repeat(64 * 1024 + 1));
        assert_eq!(
            check(fx.get(Client::Copilot, &fx.work(), "big")),
            (2, "role")
        );
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert_eq!(
            check(fx.get(Client::Copilot, &fx.work(), "big")),
            (2, "role")
        );
        // A frontmatter that never ends is refused, not read as a body.
        fx.put(&path, "---\ntools: Read\nbody\n");
        assert_eq!(
            check(fx.get(Client::Copilot, &fx.work(), "big")),
            (2, "role")
        );
        // A file with no frontmatter is all body; CRLF is read like LF.
        fx.put(&path, "---\r\ntools: Read\r\n---\r\nbody\r\n");
        let s = fx.get(Client::Copilot, &fx.work(), "big").unwrap();
        assert_eq!((s.body.as_str(), s.tools), ("body\r\n", Some(v(&["Read"]))));
    }

    /// The complete argv a launch emits: the policy the team grammar reads from it is the role-less child's.
    #[test]
    fn a_role_never_changes_the_childs_policy() {
        use crate::agents::spawn::{Recipe, client_args};
        let session = "11111111-2222-4333-8444-555555555555";
        let tokens = |c: Client| match c {
            Client::Claude => v(&["--model=haiku", "--tools=Read,Grep"]),
            Client::Copilot => v(&["--model=haiku", "--available-tools=read,shell(git)"]),
        };
        for client in [Client::Claude, Client::Copilot] {
            for (parent, ceiling) in [
                (Policy::Default, Policy::Default),
                (Policy::AllowAll, Policy::AllowAll),
                (Policy::Bypass, Policy::Bypass),
                (Policy::Default, Policy::Bypass),
            ] {
                let flags =
                    crate::agents::tree::child_flags(client, parent, ceiling, None).unwrap();
                for recipe in [
                    Recipe::First(Some("[mux-agent] task")),
                    Recipe::Fork {
                        parent: session,
                        task: Some("[mux-agent] task"),
                    },
                    Recipe::Resume(Some("notes")),
                ] {
                    let args = client_args(client, recipe, session, None);
                    assert!(
                        admit(client, &flags, &tokens(client), &args).is_ok(),
                        "{client:?} {parent:?} {ceiling:?}"
                    );
                }
            }
        }
        // A token the grammar cannot read, or one that changes the policy, refuses 2 `role`.
        let flags = v(&["--dangerously-skip-permissions", "--effort", "high"]);
        let args = v(&["--session-id", session, "[mux-agent] task"]);
        for bad in [&["--frobnicate"][..], &["--permission-mode=plan"]] {
            assert_eq!(
                check(admit(Client::Claude, &flags, &v(bad), &args)),
                (2, "role"),
                "{bad:?}"
            );
        }
        let flags = v(&["--allow-all", "--no-sandbox"]);
        let args = v(&["--session-id", session, "-i", "task"]);
        assert_eq!(
            check(admit(
                Client::Copilot,
                &flags,
                &v(&["--deny-tool=shell"]),
                &args
            )),
            (2, "role")
        );
    }
}
