# mux-agents Wiki

[简体中文](Home-zh-CN)

mux-agents is a plugin for Claude Code and GitHub Copilot CLI. When the client runs inside an rmux pane, it can start other Claude Code or Copilot CLI agents in new windows or panes of the same session, with forked context, then list, focus, message and stop them.

## Status

v0.1.0 is in progress. Today the code runs on Windows with rmux 0.10.0. tmux on Linux and macOS (#1) and the side bar (#4) are being built; neither is in the code yet.

## Design

- [Foundation](Design-Foundation) — v0.1.0 sub-project A: the rename to mux-agents, CI on Linux, macOS and Windows, and the tag-driven release
