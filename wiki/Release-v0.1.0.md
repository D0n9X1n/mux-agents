# mux-agents v0.1.0 release notes

[简体中文](https://github.com/D0n9X1n/mux-agents/wiki/Release-v0.1.0-zh-CN)

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

On Linux and macOS, .NET older than 8.0.32, 9.0.21 or 10.0.13 has an elevation-of-privilege flaw in its named mutexes ([GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2)), which mux-agents uses for its `lock` check: another local user can erase or replace file contents, loosen folder permissions or gain higher privileges, and can make mux-agent commands time out with check `lock`. Check with `[Environment]::Version` in pwsh and use a PowerShell built on a fixed .NET.

The Claude Code `Agents` pane was checked in a live session on Windows only: `/mux-agents-pane` opens it and it draws the board. Opening it after a live `spawn`, closing it by hand, and the pane on Linux and macOS are covered by tests, not by a live session.
