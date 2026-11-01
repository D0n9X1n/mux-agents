<p align="center"><img src="https://raw.githubusercontent.com/D0n9X1n/mux-agents/main/assets/icon/mux-agents.svg" width="128" height="128" alt="mux-agents icon"></p>

# mux-agents

Start Claude Code and Copilot CLI agents in tmux or rmux panes, see what each is doing, message them and stop them.

mux-agents is a plugin for Claude Code and GitHub Copilot CLI. From a chat running in a multiplexer pane, you ask for helpers; each one opens in its own window or split of the same session. An agent of the same client starts from a fork of your conversation; another client, or `-NoFork`, starts from a brief you write. A side bar shows how many are running and what each is doing. Agents talk through mailboxes on disk, never by typing into each other's panes.

## Status

0.2.0 is one native binary, `mux-agent`, built from Rust for macOS, Linux and Windows, with no PowerShell or .NET runtime to start. It keeps the commands, the state folder and the safety checks of 0.1.

The .NET named-mutex advisory [GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2), which affected the `lock` check of 0.1 on Linux and macOS, is fixed in 0.2.0: Unix locks are now files under your home folder (`~/.local/state/mux-agents/locks`, taken with `File::try_lock`), and Windows uses a named mutex `Local\MuxAgents-<hex>`.

The marketplace installs the `dist` branch, which holds the prebuilt binaries; it takes effect once the first 0.2.0 release creates `dist`.

## Quick start

Start the multiplexer, then the client inside it:

```sh
tmux        # or: rmux
claude      # or: copilot
```

Then ask in the chat, for example: "spawn a Copilot agent to write the tests for parser.rs, and open the side bar". The skill turns that into the commands below.

## Commands

Every command is `<plugin>/scripts/mux-agent <verb> …` (`<plugin>\scripts\mux-agent.cmd <verb> …` on Windows) and prints one JSON object (`-Text` prints lines instead). An agent passes free text in a file (`-TaskFile`, `-BriefFile`, `-BodyFile`), never on the command line.

| verb | example | what it does |
|---|---|---|
| `spawn` | `spawn -Client copilot -TaskFile task.md -BriefFile brief.md -Split h` | start an agent in a new window, or a split with `-Split h\|v` |
| `list` | `list -All` | live agents, what each is doing, unread messages; `-All` adds ended ones |
| `status` | `status -BodyFile step.txt` | an agent says what it is doing now |
| `send` | `send claude-1 -BodyFile note.md`, `send parent -BodyFile result.md` | post a message to an agent or to your parent |
| `read` | `read -Wait 600`, `read claude-1 -Lines 40` | wait for your messages, or read an agent's pane |
| `focus` | `focus claude-1` | switch the multiplexer to an agent's pane |
| `stop` | `stop claude-1` | close an agent's pane and its processes |
| `sidebar` | `sidebar`, `sidebar -Close` | open or close the side-bar split |

## Side bar

In Claude Code, an `Agents` pane beside the chat opens after the first successful `spawn` (or with `/mux-agents-pane`) and refreshes every 3 seconds. In any client, `sidebar` opens a narrow split that redraws every 2 seconds:

```text
mux-agents · 2 running
● claude-1  claude  4m
  writing parser tests
● copilot-1  copilot  1m
  reviewing the diff
○ copilot-2  exited 0
```

"Doing" is what the agent last reported with `status`, else the first line of its last message, else its task.

## How it works

- `spawn` opens a detached window (or `-Split h|v` splits the caller's pane) running `<binary> launcher <spec.json>` by the binary's absolute path, tags the pane with `@agent-*` options, and records the agent in the state folder: `MUX_AGENTS_STATE` if set, else `%LOCALAPPDATA%\mux-agents\state` on Windows and `$XDG_STATE_HOME/mux-agents/state` or `~/.local/state/mux-agents/state` on Linux and macOS.
- Children run `claude --dangerously-skip-permissions --effort high` or `copilot --no-sandbox --allow-all`.
- Messages go to per-pane mailboxes, never as keystrokes. `read -Wait` returns each message once. Every child posts exactly one exit notice to its parent: `exited`, `stopped` or `lost`.

## Safety checks

Agents start with permission prompts off, so mux-agents never gives an agent more than its caller already has. A `spawn` is refused, naming the check, unless:

- `sandbox`: the caller is not sandboxed.
- `policy`: the caller already runs with bypass or allow-all.
- `flags`: every client command line from the agent up to the pane uses only allowlisted flags.
- `directory`: the caller's working directory can be confirmed; the agent starts there.
- `tagged`: the caller is not itself an agent; children cannot spawn.
- `cap`: fewer than 6 agents are live in the session.

## Requirements

- macOS, Linux or Windows, x86_64 or aarch64; no PowerShell or .NET needed.
- rmux 0.10 or later on macOS, Linux or Windows, or tmux 3.2 or later on macOS or Linux, with the caller inside a pane.
- Claude Code or GitHub Copilot CLI on `PATH` (`claude` or `copilot`; `.exe` on Windows).

Inside rmux both `RMUX` and `TMUX` may be set; `RMUX` wins. Outside a pane every verb is refused with check `endpoint`.

## Install

```text
claude plugin marketplace add D0n9X1n/mux-agents
claude plugin install mux-agents@mux-agents
copilot plugin marketplace add D0n9X1n/mux-agents
copilot plugin install mux-agents@mux-agents
```

## Layout

| Path | Job |
|---|---|
| `.claude-plugin/plugin.json`, `marketplace.json` | Plugin and marketplace manifests, read by Claude Code and Copilot CLI |
| `skills/mux-agents/SKILL.md` | The skill both clients load: verbs, the file-only rule, listening etiquette |
| `scripts/mux-agent`, `scripts/mux-agent.cmd` | Entry shims: run the packaged binary for this OS and architecture |
| `src/` | The `mux-agent` binary: verbs, checks, state, locks, the process layer, the launcher and the side bar |
| `hooks/`, `types/` | The Claude Code `Agents` pane and its state contract |
| `tests/` | Rust tests, the shim, release and wiki sh tests, and `tests/claude/` for the pane |
| `wiki/` | Documentation, the source of the GitHub wiki: every page in English (`Page.md`) and Chinese (`Page-zh-CN.md`) |
| `scripts/wiki/` | Wiki publisher run by the Publish wiki workflow |
| `scripts/release/`, `scripts/dev/` | Release packaging and gates; developer benchmarks and tools |

## Test

With Rust 1.95.0:

```sh
cargo test --features test-panic --locked
cargo build --release --locked
sh tests/shim.sh
claude plugin test .
```

[CONTRIBUTING](https://github.com/D0n9X1n/mux-agents/blob/main/CONTRIBUTING.md) lists every command CI runs.

## More

- [Wiki](https://github.com/D0n9X1n/mux-agents/wiki): design pages and release notes, in English and Chinese.
- [Contributing](https://github.com/D0n9X1n/mux-agents/blob/main/CONTRIBUTING.md): layout, tests and how changes land.

MIT licensed.
