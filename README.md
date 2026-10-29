# rmux-agents

A plugin for Claude Code and GitHub Copilot CLI. When the client runs inside an RMUX pane, it can start other Claude Code or Copilot CLI agents in new windows or panes of the same session, with forked context, then list, focus, message and stop them.

## Status

Scaffold only; no code yet. The implementation is being built and reviewed in the Terminal Profiles repository (`config/rmux-agents/`, `tests/RmuxAgents*.Tests.ps1`). It moves here once part 1 (spawn, list, focus) passes its gate.

## Planned layout

| Path | Job |
|---|---|
| `.claude-plugin/plugin.json`, `marketplace.json` | Plugin and marketplace manifests, read by Claude Code and Copilot CLI |
| `skills/rmux-agents/SKILL.md` | The skill both clients load: verbs, the file-only rule, listening etiquette |
| `scripts/rmux-agent.ps1` | Entry: one JSON object per call |
| `scripts/RmuxAgents.ps1` | Library: caller, registry, locks, spawn, list, focus, send, read, stop |
| `scripts/rmux-agent-launch.ps1` | Pane launcher: waits for the launch commit, then starts the client |
| `tests/` | Pester tests on fakes, plus an isolated-server test |

## Making it general

The current code depends on Terminal Profiles in these places, each to be replaced before or during the move:

- `RmuxAgents.ps1` dot-sources `config\powershell\RmuxStore.ps1` (1012 lines; the managed RMUX store) and AST-imports `Resolve-RmuxCommand` from `SharedProfile.ps1`. Replace with plain `rmux` calls on the caller's own socket.
- State lives under `%LOCALAPPDATA%\TerminalProfiles\rmux-agents\state`, and lock mutexes are named `Local\TerminalProfiles-RmuxAgents-*`. Use an `rmux-agents` name.
- The launcher runs the clients through the Terminal Profiles `cc`/`gg` wrappers. Call `claude` and `copilot` directly, with the flags kept in one place.
- `spec.json` records the Terminal Profiles repository root. Record the plugin root.

## Requirements

- PowerShell 7.
- One of these multiplexers:
  - rmux 0.10.0 on Windows;
  - tmux on Linux or macOS. The minimum version is not settled yet.
- Claude Code or GitHub Copilot CLI.

tmux support is planned but not built yet. Inside rmux, both `RMUX` and `TMUX` are set, so `RMUX` decides which multiplexer is running.

## Install

Not installable yet. Once the code moves here:

```text
claude plugin marketplace add <path-or-repository>
claude plugin install rmux-agents@rmux-agents
copilot plugin marketplace add <path-or-repository>
copilot plugin install rmux-agents@rmux-agents
```
