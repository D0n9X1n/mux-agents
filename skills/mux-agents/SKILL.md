---
name: mux-agents
description: Start Claude Code or Copilot CLI agents in the current RMUX or tmux session with forked context, then list, focus, message and stop them.
---

## When to use

Use this skill inside an RMUX pane on Windows or a tmux 3.2+ pane on Linux or macOS; the tool refuses elsewhere.

## Entry

`<entry>` is `scripts/mux-agent` in this plugin (`scripts/mux-agent.cmd` on Windows): two folders above this file's folder (`<skill folder>/../../scripts/mux-agent`). Use the absolute path with forward slashes. A spawned agent's first message gives its exact Reply and Listen commands.

The commands below, and the Reply and Listen lines, run as-is in the Bash tool. In PowerShell, prefix `& ` (`& "<exe>" send parent …`).

## Rules

- Put free text only in files: `-TaskFile`, `-BriefFile` and `-BodyFile`.
- Agent callers always pass `-BriefFile`, because a fork can fall back to brief-only.
- Report results to your parent with `send parent`.
- As a spawned agent, run `status -BodyFile <file>` each time you start a step of your task.
- In Copilot CLI, run `sidebar` once after your first `spawn`: a narrow pane beside you shows how many agents run and what each is doing. `sidebar -Close` closes it.
- Never type into panes.
- Run `focus` only when the user asks.
- Children cannot spawn.

## Commands

```
"<entry>" spawn -Client <client> -TaskFile <task-file> -BriefFile <brief-file>
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

`spawn` also takes `-Name <name>`, `-Split h|v` (split your pane instead of a new window) and `-NoFork`.

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
- `policy`: the caller's live policy is unreadable or is neither bypass nor allow-all.
- `flags`: a flag outside the allowlist, or an unreadable command line, on any `claude` or `copilot` process from the agent up to the pane.
- `directory`: the caller's launch directory cannot be confirmed.
- `tagged`: a spawn or a side bar from an agent pane; children cannot spawn or open the side bar.
- `cap`: 6 live agents in the session.
- `name`: a bad or taken name.
- Other check names: `brief`, `target`, `parent`, `endpoint`, `state`, `lock`, `caller`, `usage` and `internal`.
