# mux-agents v0.2.1 release notes

[简体中文](https://github.com/D0n9X1n/mux-agents/wiki/Release-v0.2.1-zh-CN)

Start Claude Code and Copilot CLI agents in tmux or rmux panes, see what each is doing, message them and stop them.

## What changed

- **Tabs or panes, per session.** `placement tab|h|v` chooses where `spawn` opens agents for the rest of the session: `tab` (a new window, the default) or `h`/`v` (a split beside you). `placement` alone shows the choice. It is a multiplexer session option, so it ends with the session and nothing is written to disk.
- `spawn -Tab` (new) or `-Split h|v` overrides the session's choice for one spawn. The spawn reply adds `placement` and `placementSource` (`flag`, `session` or `default`).
- **The side bar opens on its own.** After each successful `spawn`, the session's bar is found or opened beside you; there is still one bar per session. `sidebar -Close` keeps it closed for the session and `sidebar` opens it again.
- A side-bar failure never fails a spawn: the agent is already started, and the reply carries `sidebar: null` and `sidebarError`.
- Ask in your chat: *"open agents as tabs first"* or *"put agents in panes first"*.

## Install

```text
claude plugin marketplace add D0n9X1n/mux-agents
claude plugin install mux-agents@mux-agents
copilot plugin marketplace add D0n9X1n/mux-agents
copilot plugin install mux-agents@mux-agents
```

## Upgrade

From 0.2.0, update with one line per client and restart your sessions:

```text
claude plugin marketplace update mux-agents && claude plugin update mux-agents@mux-agents
copilot plugin marketplace update mux-agents && copilot plugin update mux-agents@mux-agents
```

The state folder and its record, mail and lock formats are unchanged within 0.2.x. Agent panes and the side bar keep the binary they started with until you restart them.

## Known limits

In tab mode the side bar stays in the window you work in; it is not added to every agent's tab.
