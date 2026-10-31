# mux-agents Wiki

[简体中文](Home-zh-CN)

mux-agents is a plugin for Claude Code and GitHub Copilot CLI. When the client runs inside an rmux or tmux pane, it can start other Claude Code or Copilot CLI agents in new windows or panes of the same session, with forked context, then list, focus, message and stop them.

## Status

v0.1.0 is in progress. The code runs on Windows with rmux 0.10.0 and on Linux and macOS with tmux 3.2 or later. The side bar (#4) is being built and is not in the code yet.

On Linux and macOS with .NET older than 8.0.32, 9.0.21 or 10.0.13, another local user can make mux-agent commands time out with check lock. Nothing leaks and no state is damaged. Use a current PowerShell.

## Design

- [Foundation](Design-Foundation) — v0.1.0 sub-project A: the rename to mux-agents, CI on Linux, macOS and Windows, and the tag-driven release
