# mux-agents Wiki

[简体中文](Home-zh-CN)

mux-agents is a plugin for Claude Code and GitHub Copilot CLI. When the client runs inside an rmux or tmux pane, it can start other Claude Code or Copilot CLI agents in new windows or panes of the same session, with forked context, then list, focus, message and stop them.

## Status

0.2.0 is one native Rust binary, `mux-agent`, on macOS, Linux and Windows, with rmux 0.10 or later on any of them or tmux 3.2 or later on macOS and Linux. It needs no PowerShell or .NET. It shows a side bar (#4): a multiplexer split for every client and a pane in Claude Code.

The .NET named-mutex advisory [GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2), which affected the `lock` check of 0.1 on Linux and macOS, is fixed in 0.2.0: Unix locks are files under `~/.local/state/mux-agents/locks`, and Windows uses a named mutex `Local\MuxAgents-<hex>`.

## Use

- [Usage](Usage) — install, commands, side bar, safety checks, requirements, upgrades and troubleshooting

## Design

- [Foundation](Design-Foundation) — v0.1.0 sub-project A: the rename to mux-agents, CI on Linux, macOS and Windows, and the tag-driven release; its Rust (0.2.0) section covers the binary, the locks and the `dist` branch
- [Cross-platform](Design-Cross-Platform) — v0.1.0 sub-project B: tmux on Linux and macOS, rmux on Windows, with the same checks; its Rust (0.2.0) section adds rmux on every OS
- [Side bar](Design-Side-Bar) — v0.1.0 sub-project C: the agent count and what each agent is doing, in a multiplexer split and in a Claude Code pane
- [Runtime check](Design-Runtime-Check) — issue #25: on Linux and macOS, refuse a .NET affected by GHSA-2j93-m82w-73j2; 0.2.0 runs no .NET and drops the check

## Releases

- [v0.2.0](Release-v0.2.0) — one native binary for macOS, Linux and Windows, no PowerShell; file locks on Unix
- [v0.1.0](Release-v0.1.0) — the first release: spawn, list, status, send, read, focus, stop and the side bar on Windows, Linux and macOS
