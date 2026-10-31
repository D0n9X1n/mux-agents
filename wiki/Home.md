# mux-agents Wiki

[简体中文](Home-zh-CN)

mux-agents is a plugin for Claude Code and GitHub Copilot CLI. When the client runs inside an rmux or tmux pane, it can start other Claude Code or Copilot CLI agents in new windows or panes of the same session, with forked context, then list, focus, message and stop them.

## Status

v0.1.0 runs on Windows with rmux 0.10.0 and on Linux and macOS with tmux 3.2 or later. It shows a side bar (#4): a multiplexer split for every client and a pane in Claude Code.

On Linux and macOS, .NET older than 8.0.32, 9.0.21 or 10.0.13 has an elevation-of-privilege flaw in its named mutexes ([GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2)), which mux-agents uses for its `lock` check: another local user can erase or replace file contents, loosen folder permissions or gain higher privileges, and can make mux-agent commands time out with check `lock`. Check with `[Environment]::Version` in pwsh and use a PowerShell built on a fixed .NET.

## Design

- [Foundation](Design-Foundation) — v0.1.0 sub-project A: the rename to mux-agents, CI on Linux, macOS and Windows, and the tag-driven release
- [Cross-platform](Design-Cross-Platform) — v0.1.0 sub-project B: tmux on Linux and macOS, rmux on Windows, with the same checks
- [Side bar](Design-Side-Bar) — v0.1.0 sub-project C: the agent count and what each agent is doing, in a multiplexer split and in a Claude Code pane

## Releases

- [v0.1.0](Release-v0.1.0) — the first release: spawn, list, status, send, read, focus, stop and the side bar on Windows, Linux and macOS
