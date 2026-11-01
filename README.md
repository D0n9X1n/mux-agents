<p align="center"><img src="https://raw.githubusercontent.com/D0n9X1n/mux-agents/main/assets/icon/mux-agents.svg" width="128" height="128" alt="mux-agents icon"></p>

# mux-agents

Start Claude Code and Copilot CLI agents in tmux or rmux panes, see what each is doing, message them and stop them.

<p align="center">
  <a href="https://github.com/D0n9X1n/mux-agents/actions/workflows/ci.yml"><img src="https://github.com/D0n9X1n/mux-agents/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/D0n9X1n/mux-agents/releases/latest"><img src="https://img.shields.io/github/v/release/D0n9X1n/mux-agents" alt="latest release"></a>
</p>

<p align="center"><img src="https://raw.githubusercontent.com/D0n9X1n/mux-agents/main/assets/preview/mux-agents-preview.png" alt="Your Claude Code chat on the left, three helper agents in their own panes, and the mux-agents side bar showing what each is doing"></p>

## Features

- **Helpers in panes you can see.** Ask in your chat; each agent opens in its own window or split of your session.
- **They start with your context.** A same-client agent forks your conversation; any other agent starts from a brief you write.
- **A live side bar.** How many agents run and what each is doing right now.
- **Mailboxes, not keystrokes.** Agents report back through files; nothing ever types into another pane.
- **Safe by default.** An agent never gets more than its caller already has; a refused spawn names the reason.
- **Fast and native.** One small Rust binary on macOS, Linux and Windows; `list` with six agents takes about 23 ms, and no PowerShell or .NET is needed.
- **tmux or rmux.** tmux 3.2+ on macOS and Linux, rmux 0.10+ everywhere.

## Quick start

Install in Claude Code, Copilot CLI or both. The install carries prebuilt binaries for every platform, so there is nothing to build or download:

```sh
claude plugin marketplace add D0n9X1n/mux-agents && claude plugin install mux-agents@mux-agents
copilot plugin marketplace add D0n9X1n/mux-agents && copilot plugin install mux-agents@mux-agents
```

Start the client inside tmux or rmux, then just ask:

```sh
tmux                                   # or: rmux
claude --dangerously-skip-permissions  # or: copilot --allow-all
```

> spawn a Copilot agent to review the diff, a Claude agent to write the parser tests, and open the side bar

Upgrade with one line:

```sh
claude plugin marketplace update mux-agents && claude plugin update mux-agents@mux-agents
copilot plugin marketplace update mux-agents && copilot plugin update mux-agents@mux-agents
```

## Learn more

- [Usage](https://github.com/D0n9X1n/mux-agents/wiki/Usage): commands, side bar, safety checks, requirements, upgrades and troubleshooting.
- [Wiki](https://github.com/D0n9X1n/mux-agents/wiki): design pages and release notes, in English and 简体中文.
- [Contributing](https://github.com/D0n9X1n/mux-agents/blob/main/CONTRIBUTING.md) · MIT licensed.
