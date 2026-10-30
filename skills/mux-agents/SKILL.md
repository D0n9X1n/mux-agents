---
name: mux-agents
description: Start Claude Code or Copilot CLI agents in the current RMUX session with forked context, then list, focus, message and stop them.
---

## When to use

Use this skill inside an RMUX pane on Windows; the tool refuses elsewhere.

## Entry

`<entry>` is `scripts/mux-agent.ps1` in this plugin: two folders above this file's folder (`<skill folder>/../../scripts/mux-agent.ps1`). Use the absolute path with forward slashes. A spawned agent's first message gives its exact Reply and Listen commands.

## Rules

- Put free text only in files: `-TaskFile`, `-BriefFile` and `-BodyFile`.
- Agent callers always pass `-BriefFile`, because a fork can fall back to brief-only.
- Report results to your parent with `send parent`.
- Never type into panes.
- Run `focus` only when the user asks.
- Children cannot spawn.

## Commands

```
pwsh -NoProfile -File "<entry>" spawn -Client <client> -TaskFile <task-file> -BriefFile <brief-file>
pwsh -NoProfile -File "<entry>" list
pwsh -NoProfile -File "<entry>" focus <name>
pwsh -NoProfile -File "<entry>" send <name> -BodyFile <body-file>
pwsh -NoProfile -File "<entry>" send parent -BodyFile <body-file>
pwsh -NoProfile -File "<entry>" read -Wait <seconds>
pwsh -NoProfile -File "<entry>" read <name> -Lines <n>
pwsh -NoProfile -File "<entry>" stop <name>
```

`spawn` also takes `-Name <name>`, `-Split h|v` (split your pane instead of a new window) and `-NoFork`.

## Listening

- Claude Code: run `read -Wait 600` as a background command and re-arm it after each wake.
- Copilot CLI: run `read -Wait 600` at the end of each task.
- A child that finishes its task keeps listening until its parent stops it.

## Output

- Every command prints one JSON object.
- Exit code 0 means ok, 1 means error, and 2 means refused or usage.
- On failure, `check` names the cause.

## Refusals

- `sandbox`: the caller's token is unreadable, AppContainer, restricted or below Medium integrity.
- `policy`: the caller's live policy is unreadable or is neither bypass nor allow-all.
- `flags`: a flag outside the allowlist on any `claude.exe` or `copilot.exe` command line from the agent up to the pane.
- `directory`: the caller's launch directory cannot be confirmed.
- `tagged`: a spawn from an agent pane; children cannot spawn.
- `cap`: 6 live agents in the session.
- `name`: a bad or taken name.
- Other check names: `brief`, `target`, `parent`, `endpoint`, `state` and `lock`.
