---
name: mux-agents
description: Terminal CLI only: needs a tmux or rmux pane. Start Claude Code or Copilot CLI agents in the current RMUX or tmux session with forked context, then list, focus, message and stop them.
---

## When to use

Use this skill inside an rmux 0.10+ pane (Windows, Linux or macOS) or a tmux 3.2+ pane (Linux or macOS); the tool refuses elsewhere.

Skip it in the Claude Code desktop app, web or cloud sessions, IDE chat panels, the GitHub Copilot app and headless or SDK runs, and use the host's normal workflow instead. Never start a multiplexer or set variables to make it apply. An IDE's integrated terminal running the CLI inside tmux or rmux is fine.

## Entry

`<entry>` is `scripts/mux-agent` in this plugin (`scripts/mux-agent.cmd` on Windows): two folders above this file's folder (`<skill folder>/../../scripts/mux-agent`). Use the absolute path with forward slashes. A spawned agent's first message gives its exact Reply and Listen commands.

The commands below, and the Reply and Listen lines, run as-is in the Bash tool. In PowerShell, prefix `& ` (`& "<exe>" send parent …`).

## Rules

- Put free text only in files: `-TaskFile`, `-BriefFile` and `-BodyFile`.
- Always pass `-Name <short-kebab-name>` that says what the agent does (for example `add-tests`): 1-24 lowercase letters, digits or hyphens, starting with a letter. A name is never reused in the session; a taken one is refused with a free one to try.
- Agent callers always pass `-BriefFile`, because a fork can fall back to brief-only.
- Report results to your parent with `send parent`.
- As a spawned agent, run `status -BodyFile <file>` each time you start a step of your task.
- When the user wants agents as tabs first, run `placement tab`; as panes first, run `placement h` (or `v`). It lasts for the session.
- The first `spawn` opens a narrow side bar beside you showing how many agents run and what each is doing. `sidebar -Close` closes it for the session; `sidebar` opens it again.
- Never type into panes.
- Run `focus` only when the user asks.
- Children cannot spawn.

## Commands

```
"<entry>" spawn -Client <client> -Name <name> -TaskFile <task-file> -BriefFile <brief-file>
"<entry>" placement tab
"<entry>" placement h
"<entry>" placement
"<entry>" list
"<entry>" list -All
"<entry>" focus <name>
"<entry>" send <name> -BodyFile <body-file>
"<entry>" send parent -BodyFile <body-file>
"<entry>" status -BodyFile <status-file>
"<entry>" read -Wait <seconds>
"<entry>" read <name> -Lines <n>
"<entry>" stop <name>
"<entry>" sidebar
"<entry>" sidebar -Close
```

`spawn` opens where `placement` says (a new window by default), titled with the agent's name. It also takes `-Split h|v` or `-Tab` (this spawn only, over the placement) and `-NoFork`. Pass `-Model <model>` (for example `sonnet`, `opus` or `sonnet[1m]`) only when the user names a model; without it the agent uses its client's default.

`list` shows each live agent's state, what it is doing and its unread count; `list -All` adds the ended agents (at most 10, newest first).

## Listening

- Claude Code: run `read -Wait 600` as a background command and re-arm it after each wake.
- Copilot CLI: run `read -Wait 600` at the end of each task.
- A child that finishes its task keeps listening until its parent stops it.

## Output

- Every command prints one JSON object.
- Exit code 0 means ok, 1 means error, and 2 means refused or usage.
- On failure, `check` names the cause.

## Refusals

- `entry`: no usable `mux-agent` binary for this OS and architecture.
- `sandbox`: the caller runs in a sandbox: `SANDBOX_RUNTIME=1`, a macOS sandbox, Linux `NoNewPrivs`, or a Windows token that is unreadable, AppContainer, restricted or below Medium integrity.
- `policy`: the caller's live policy is unreadable or is neither bypass nor allow-all. For Claude Code, `reason` says whether the transcript or the screen failed and why (for example `screen: no mode line`).
- `flags`: a flag outside the allowlist, or an unreadable command line, on any `claude` or `copilot` process from the agent up to the pane.
- `directory`: the caller's launch directory cannot be confirmed.
- `tagged`: a spawn or a side bar from an agent pane; children cannot spawn or open the side bar.
- `cap`: 6 live agents in the session.
- `name`: a bad or taken name.
- Other check names: `brief`, `target`, `parent`, `endpoint`, `state`, `lock`, `caller`, `usage` and `internal`.
