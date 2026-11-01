# Using mux-agents

[简体中文](Usage-zh-CN)

The details behind the [README](https://github.com/D0n9X1n/mux-agents#readme): how to install, what each command does, the side bar, the safety checks, upgrades and what to do when a spawn is refused.

## Install

```text
claude plugin marketplace add D0n9X1n/mux-agents
claude plugin install mux-agents@mux-agents
copilot plugin marketplace add D0n9X1n/mux-agents
copilot plugin install mux-agents@mux-agents
```

The marketplace installs the `dist` branch, which each release fills with the prebuilt `mux-agent` binary for macOS, Linux and Windows on x86_64 and aarch64, plus their `SHA256SUMS`. The entry `scripts/mux-agent` (`scripts\mux-agent.cmd` on Windows) picks the one for your machine.

To run a build from source in Claude Code instead, clone into the skills folder and build there: `git clone https://github.com/D0n9X1n/mux-agents ~/.claude/skills/mux-agents`, then `cargo build --release` in that folder. The next session loads it as `mux-agents@skills-dir`, and the entry finds `target/release/mux-agent`. Don't install the marketplace plugin beside it: an installed marketplace plugin of the same name takes precedence.

## How a spawn flows

```mermaid
flowchart LR
  you[You ask in the chat] --> skill[mux-agents skill]
  skill --> entry["scripts/mux-agent spawn"]
  entry --> checks{"Safety checks"}
  checks -- refused --> reply["JSON reply naming the check"]
  checks -- passed --> pane["New tmux or rmux pane"]
  pane --> launcher["mux-agent launcher"]
  launcher --> agent["claude or copilot, forked or briefed"]
  agent -- status and send --> mail[("Mailboxes and records on disk")]
  mail --> side["Side bar and list"]
  mail -- read -Wait --> you
```

## Commands

You rarely type these: the skill runs them for you. Every command is `<plugin>/scripts/mux-agent <verb> …` (`<plugin>\scripts\mux-agent.cmd <verb> …` on Windows) and prints one JSON object (`-Text` prints lines instead). An agent passes free text in a file (`-TaskFile`, `-BriefFile`, `-BodyFile`), never on the command line.

| verb | example | what it does |
|---|---|---|
| `spawn` | `spawn -Client copilot -Name add-tests -TaskFile task.md -BriefFile brief.md -Split h` | start an agent where the session's `placement` says (a new window by default); `-Split h\|v` or `-Tab` decides for this one; `-Model <model>` picks its model (else the client's default) |
| `placement` | `placement h`, `placement tab` | where `spawn` opens agents for the rest of the session: `tab` (a new window, the default), `h` or `v` (a split beside you) |
| `list` | `list -All` | live agents, what each is doing, unread messages; `-All` adds ended ones |
| `status` | `status -BodyFile step.txt` | an agent says what it is doing now |
| `send` | `send claude-1 -BodyFile note.md`, `send parent -BodyFile result.md` | post a message to an agent or to your parent |
| `read` | `read -Wait 600`, `read claude-1 -Lines 40` | wait for your messages, or read an agent's pane |
| `focus` | `focus claude-1` | switch the multiplexer to an agent's pane |
| `stop` | `stop claude-1` | close an agent's pane and its processes; an agent that already ended answers `ok` with `already` (`exited`, `stopped` or `lost`) |
| `sidebar` | `sidebar`, `sidebar -Close` | reopen the side-bar split, or close it for the rest of the session |

Exit code 0 means ok, 1 an error and 2 a refusal or usage error; on failure, `check` names the cause.

Choose where agents open once per session: `placement h` (or `v`) splits your pane for every later `spawn`, `placement tab` goes back to new windows, and `placement` alone shows the choice. It lives on the multiplexer session, so it ends with the session and nothing is written to disk; `-Split h|v` or `-Tab` on one `spawn` overrides it.

Every agent's name says what it does and is its tab's name or its pane's title. The skill always passes `-Name <short-kebab-name>` (1-24 lowercase letters, digits or hyphens, starting with a letter; `parent`, `root` and `all` are reserved). Without `-Name`, the name comes from the task's first words: "Please add a subtract function to calc.py" becomes `add-subtract-function`, and a task with no usable words gets `claude-1` or `copilot-1`. A name is never reused in its session, even after its agent ended: a taken derived name gets `-2`, `-3`, … (cut at a word boundary to stay within 24), and a taken `-Name` is refused with a free one to try.

## Side bar

In Claude Code, an `Agents` pane beside the chat opens after the first successful `spawn` (or with `/mux-agents-pane`) and refreshes every 3 seconds. In any client, the first successful `spawn` also opens a split 42 columns wide beside you that redraws every 2 seconds; later spawns reuse it, `sidebar -Close` closes it for the rest of the session and `sidebar` opens it again:

```text
mux-agents · 2 running
● claude-1  claude  4m
  running the parser tests (14/20 pass)
● copilot-1  copilot  1m
  reviewing src/spawn.rs
○ claude-2  exited 0
```

"Doing" is what the agent last reported with `status`, else the first line of its last message, else its task.

In tab mode the bar stays in the window you spawned from, not in each agent's window. `sidebar -Close` closes only this multiplexer bar; the Claude Code `Agents` pane is separate.

## Safety checks

Agents start with permission prompts off, so mux-agents never gives an agent more than its caller already has. A `spawn` is refused, naming the check, unless:

- `sandbox`: the caller is not sandboxed.
- `policy`: the caller already runs with bypass or allow-all.
- `flags`: every client command line from the agent up to the pane uses only allowlisted flags.
- `directory`: the caller's working directory can be confirmed; the agent starts there.
- `tagged`: the caller is not itself an agent; children cannot spawn.
- `cap`: fewer than 6 agents are live in the session.

Children run `claude --dangerously-skip-permissions --effort high` or `copilot --no-sandbox --allow-all`, in the caller's directory.

## Requirements

- macOS, Linux or Windows, x86_64 or aarch64; no PowerShell or .NET needed.
- rmux 0.10 or later on macOS, Linux or Windows, or tmux 3.2 or later on macOS or Linux, with the caller inside a pane.
- Claude Code or GitHub Copilot CLI on `PATH` (`claude` or `copilot`; `.exe` on Windows).

Inside rmux both `RMUX` and `TMUX` may be set; `RMUX` wins.

## Upgrade

Close what runs, update, then restart:

1. Close the side bar with `sidebar -Close`.
2. `stop` every agent (`list` shows them).
3. Update the plugin in each client you use, with one line:

```text
claude plugin marketplace update mux-agents && claude plugin update mux-agents@mux-agents
copilot plugin marketplace update mux-agents && copilot plugin update mux-agents@mux-agents
```

Refresh the marketplace first: Claude Code's plugin update reads the catalog it cached when you added the marketplace, so on its own it can report the old version as the latest.

4. Restart your Claude Code and Copilot CLI sessions, then spawn agents and open the side bar again.

Why: each agent pane and the side bar run the binary by the absolute path they started with, so they keep the old version until restarted. From 0.1 the order matters most: the 0.1 named mutex and the 0.2 lock files do not serialize, so a 0.1 pane and a 0.2 command must never run at the same time. Within 0.2.x the state folder, the record, mail and lock file formats stay the same.

Claude Code turns background auto-update off by default for a third-party marketplace such as this one. To turn it on, open `/plugin`, go to **Marketplaces**, select `mux-agents` and select **Enable auto-update**. Claude Code then, within ten minutes of your first message in a session, refreshes the marketplace and updates the plugin on disk; panes already running keep their binary until you restart them.

## Troubleshooting

Every refusal is one JSON line with `check` naming the cause:

| check | what it means | what to do |
|---|---|---|
| `entry` | no `mux-agent` binary for this OS and architecture | reinstall the plugin from the marketplace, or build from source as above |
| `endpoint` | not inside a multiplexer pane, or tmux older than 3.2 or rmux older than 0.10 | start the client inside tmux 3.2+ or rmux 0.10+ |
| `sandbox` | the caller runs in a sandbox (`SANDBOX_RUNTIME=1`, a macOS sandbox, Linux `NoNewPrivs`, a restricted Windows token) | run the client outside the sandbox |
| `policy` | the caller is not in bypass or allow-all; for Claude Code, `reason` says which half failed: `transcript: no session id`, `transcript: not found`, `transcript: no mode`, `transcript: other mode`, `screen: unreadable`, `screen: no mode line` or `screen: other mode` | Claude Code: bypass permissions (`--dangerously-skip-permissions` or Shift+Tab), with the footer badge `⏵⏵ bypass permissions on` in the pane's last 8 rows; Copilot CLI: `--allow-all` |
| `cap` | 6 agents already live in this session | `stop` one |
| `tagged` | an agent tried to spawn, open the side bar or set `placement` | only the top-level chat spawns |

## How it works

- `spawn` opens a detached window (or, by `-Split h|v` or the session's `placement`, splits the caller's pane) running `<binary> launcher <spec.json>` by the binary's absolute path, tags the pane with `@agent-*` options, and records the agent in the state folder: `MUX_AGENTS_STATE` if set, else `%LOCALAPPDATA%\mux-agents\state` on Windows and `$XDG_STATE_HOME/mux-agents/state` or `~/.local/state/mux-agents/state` on Linux and macOS.
- Once the agent is committed, `spawn` finds or opens the session's side bar under the side bar's own lock; a failure there is reported as `sidebarError` and never fails the spawn.
- Messages go to per-pane mailboxes, never as keystrokes. `read -Wait` returns each message once. Every child posts exactly one exit notice to its parent: `exited`, `stopped` or `lost`.
- An agent whose tab or pane is gone is closed on the next `list`, `read`, `spawn`, `stop` or side-bar tick: one `lost` notice (or `exited` with its code), gone from `list` and the side bar, kept in `list -All` with `closedBy: "pane"`. A pane that `remain-on-exit` keeps after its agent ended is closed too, only while it is still tagged as that agent.
- Locks are per user: on Unix, files under `~/.local/state/mux-agents/locks`; on Windows, a named mutex `Local\MuxAgents-<hex>`. This fixes the .NET named-mutex advisory [GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2) that affected 0.1 on Linux and macOS.
- `list` with six live agents averages about 23 ms (`scripts/dev/bench-list.sh`, tmux 3.8 on an Apple silicon Mac).
