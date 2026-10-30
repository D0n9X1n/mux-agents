# mux-agents

A plugin for Claude Code and GitHub Copilot CLI. When the client runs inside an RMUX pane, it can start other Claude Code or Copilot CLI agents in new windows or panes of the same session, with forked context, then list, focus, message and stop them.

## Status

0.1.0 runs on Windows with rmux 0.10.0. tmux on Linux is planned, not built. The code moved here from the Terminal Profiles repository and no longer depends on it.

## Layout

| Path | Job |
|---|---|
| `.claude-plugin/plugin.json`, `marketplace.json` | Plugin and marketplace manifests, read by Claude Code and Copilot CLI |
| `skills/mux-agents/SKILL.md` | The skill both clients load: verbs, the file-only rule, listening etiquette |
| `scripts/mux-agent.ps1` | Entry: one verb per call, one JSON object out |
| `scripts/MuxAgents.ps1` | Library: caller, checks, registry, locks, spawn, list, focus, send, read, stop, launcher |
| `scripts/mux-agent-launcher.ps1` | Pane launcher: waits for the launch commit, then starts the client |
| `tests/` | Pester 4.10.1 tests on fakes; `Invoke-MuxAgentTests.ps1` runs them |

## How it works

- `spawn` opens a detached window (or `-Split h|v` splits the caller's pane) running the launcher, tags the pane with `@agent-*` options, and records the agent under `%LOCALAPPDATA%\mux-agents\state\<server key>\` (`MUX_AGENTS_STATE` overrides it). A same-client child forks the caller's session; otherwise it starts brief-only.
- Children run `claude --dangerously-skip-permissions --effort high` or `copilot --no-sandbox --allow-all`. So an agent may spawn only when its own token is not sandboxed, its live policy is full bypass, its client command lines carry only allowlisted flags, and its launch directory is confirmed; children start in that directory.
- Messages go to per-pane mailboxes, never as keystrokes. `read -Wait` returns each message once. Every child posts exactly one exit notice to its parent: `exited`, `stopped` or `lost`.
- At most 6 agents per session; children cannot spawn.

## Requirements

- Windows, PowerShell 7.4 or later.
- rmux 0.10.0, with the caller inside an rmux pane.
- Claude Code or GitHub Copilot CLI on `PATH` as `claude.exe` or `copilot.exe`.

Inside rmux both `RMUX` and `TMUX` are set; `RMUX` wins. Other platforms are refused with check `platform`.

## Install

```text
claude plugin marketplace add <path-or-repository>
claude plugin install mux-agents@mux-agents
copilot plugin marketplace add <path-or-repository>
copilot plugin install mux-agents@mux-agents
```

## Test

Install Pester 4.10.1 and powershell-yaml 0.4.12 once, then run the tests:

```powershell
Install-Module Pester -RequiredVersion 4.10.1 -Scope CurrentUser
Install-Module powershell-yaml -RequiredVersion 0.4.12 -Scope CurrentUser
pwsh -NoProfile -File tests/Invoke-MuxAgentTests.ps1
```

Off Windows the suites tagged `windows` are skipped. The script exits 1 when any test fails.
