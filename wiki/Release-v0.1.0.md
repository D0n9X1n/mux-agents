# mux-agents v0.1.0 release notes

[简体中文](Release-v0.1.0-zh-CN)

Start Claude Code and Copilot CLI agents in tmux or rmux panes, see what each is doing, message them and stop them.

## What it does

- `spawn` starts a Claude Code or Copilot CLI agent in a new window or split of your multiplexer session. An agent of the same client starts from a fork of your conversation; another client, or `-NoFork`, starts from a brief you write.
- `list`, `focus`, `send`, `read` and `stop` manage the agents. Messages go through mailboxes on disk, never as keystrokes.
- `status` lets an agent say what it is doing. `list -All` adds agents that ended.
- The side bar shows how many agents are running and what each is doing: a pane beside the chat in Claude Code, or `sidebar` as a narrow split in any client.

## Platforms

- Windows with rmux 0.10.0.
- Linux and macOS with tmux 3.2 or later.
- PowerShell 7.4 or later, and Claude Code or GitHub Copilot CLI on `PATH`.

## Safety checks

Agents start with permission prompts off, so `spawn` is refused unless the caller is not sandboxed (`sandbox`), already runs with bypass or allow-all (`policy`), uses only allowlisted flags (`flags`), has a confirmed working directory (`directory`), is not itself an agent (`tagged`), and fewer than 6 agents are live (`cap`).

## Install

```text
claude plugin marketplace add D0n9X1n/mux-agents
claude plugin install mux-agents@mux-agents
copilot plugin marketplace add D0n9X1n/mux-agents
copilot plugin install mux-agents@mux-agents
```

The release also carries `mux-agents-v0.1.0.zip`, the plugin files of this tag.

## Known limits

On Linux and macOS with .NET older than 8.0.32, 9.0.21 or 10.0.13, another local user can make mux-agent commands time out with check `lock`. Nothing leaks and no state is damaged. Use a current PowerShell.

The Claude Code `Agents` pane was checked in a live session on Windows only: `/mux-agents-pane` opens it and it draws the board. Opening it after a live `spawn`, closing it by hand, and the pane on Linux and macOS are covered by tests, not by a live session.
