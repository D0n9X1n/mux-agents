# mux-agents Wiki

[简体中文](Home-zh-CN)

mux-agents is a plugin for Claude Code and GitHub Copilot CLI. When the client runs inside an rmux or tmux pane, it can start other Claude Code or Copilot CLI agents in new windows or panes of the same session, with forked context, then list, focus, message and stop them.

## Status

v0.1.0 runs on Windows with rmux 0.10.0 and on Linux and macOS with tmux 3.2 or later. It shows a side bar (#4): a multiplexer split for every client and a pane in Claude Code.

On Linux and macOS, .NET 8.0.0–8.0.31, 9.0.0–9.0.20, 10.0.0–10.0.12 and 11.0 previews through RC 1 have an elevation-of-privilege flaw in their named mutexes ([GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2)), which mux-agents uses for its `lock` check: another local user can erase or replace file contents, loosen folder permissions or gain higher privileges. mux-agent after v0.1.0 refuses such a .NET with check `runtime`; the v0.1.0 release does not check. Install PowerShell 7.4.21, 7.5.12, 7.6.7 or later, or set `MUX_AGENTS_ALLOW_AFFECTED_DOTNET=1` to run anyway; replies then carry `advisory`.

## Design

- [Foundation](Design-Foundation) — v0.1.0 sub-project A: the rename to mux-agents, CI on Linux, macOS and Windows, and the tag-driven release
- [Cross-platform](Design-Cross-Platform) — v0.1.0 sub-project B: tmux on Linux and macOS, rmux on Windows, with the same checks
- [Side bar](Design-Side-Bar) — v0.1.0 sub-project C: the agent count and what each agent is doing, in a multiplexer split and in a Claude Code pane
- [Runtime check](Design-Runtime-Check) — issue #25: on Linux and macOS, refuse a .NET affected by GHSA-2j93-m82w-73j2 unless MUX_AGENTS_ALLOW_AFFECTED_DOTNET=1

## Releases

- [v0.1.0](Release-v0.1.0) — the first release: spawn, list, status, send, read, focus, stop and the side bar on Windows, Linux and macOS
