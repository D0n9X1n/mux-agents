<p align="center"><img src="https://raw.githubusercontent.com/D0n9X1n/mux-agents/main/assets/icon/mux-agents.svg" width="128" height="128" alt="mux-agents icon"></p>

# mux-agents

Start Claude Code and Copilot CLI agents in tmux or rmux panes, see what each is doing, message them and stop them.

<p align="center">
  <a href="https://github.com/D0n9X1n/mux-agents/actions/workflows/ci.yml"><img src="https://github.com/D0n9X1n/mux-agents/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/D0n9X1n/mux-agents/releases/latest"><img src="https://img.shields.io/github/v/release/D0n9X1n/mux-agents?color=34d399" alt="Release"></a>
  <a href="https://github.com/D0n9X1n/mux-agents/blob/main/LICENSE"><img src="https://img.shields.io/github/license/D0n9X1n/mux-agents?color=60a5fa" alt="License: MIT"></a>
  <a href="https://github.com/D0n9X1n/mux-agents/wiki/Usage#requirements"><img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-8b95a7" alt="Platforms: macOS, Linux, Windows"></a>
  <a href="https://github.com/D0n9X1n/mux-agents/blob/main/Cargo.toml"><img src="https://img.shields.io/badge/rust-1.89%2B-dea584?logo=rust&amp;logoColor=white" alt="Rust 1.89+"></a>
  <br>
  <a href="https://github.com/tmux/tmux"><img src="https://img.shields.io/badge/tmux-3.2%2B-1bb91f" alt="tmux 3.2+"></a>
  <a href="https://github.com/D0n9X1n/mux-agents/wiki/Usage#requirements"><img src="https://img.shields.io/badge/rmux-0.10%2B-a78bfa" alt="rmux 0.10+"></a>
  <a href="https://github.com/D0n9X1n/mux-agents#quick-start"><img src="https://img.shields.io/badge/Claude%20Code-plugin-d97757" alt="Claude Code plugin"></a>
  <a href="https://github.com/D0n9X1n/mux-agents#quick-start"><img src="https://img.shields.io/badge/Copilot%20CLI-plugin-24292f?logo=githubcopilot&amp;logoColor=white" alt="Copilot CLI plugin"></a>
</p>

<p align="center"><img src="https://raw.githubusercontent.com/D0n9X1n/mux-agents/main/assets/preview/mux-agents-preview.png" alt="Your Claude Code chat on the left, three helper agents in their own panes, and the mux-agents side bar showing what each is doing"></p>

## Features

- **Helpers in panes you can see.** Ask in your chat; choose once per session whether agents open as tabs or as panes beside you, and the side bar opens on its own.
- **They start with your context.** A same-client agent forks your conversation; any other agent starts from a brief you write.
- **A live side bar.** How many agents run and what each is doing right now.
- **Mailboxes, not keystrokes.** Agents report back through files; nothing ever types into another pane.
- **Safe by default.** An agent never gets more than its caller already has; a refused spawn names the reason.
- **Fast and native.** One small Rust binary on macOS, Linux and Windows; `list` with six agents takes about 23 ms, and no PowerShell or .NET is needed.
- **tmux or rmux.** tmux 3.2+ on macOS and Linux, rmux 0.10+ everywhere.

<table>
  <tr>
    <td width="50%"><img src="https://raw.githubusercontent.com/D0n9X1n/mux-agents/main/assets/preview/mux-agents-sidebar.png" alt="The side bar: three running agents with what each is doing, one exited"></td>
    <td width="50%"><img src="https://raw.githubusercontent.com/D0n9X1n/mux-agents/main/assets/preview/mux-agents-list.png" alt="mux-agent list answering one JSON object with each agent's state and current step"></td>
  </tr>
  <tr>
    <td width="50%"><img src="https://raw.githubusercontent.com/D0n9X1n/mux-agents/main/assets/preview/mux-agents-mail.png" alt="An agent sends its report to the parent through a file, and read -Wait returns it with the exit notice"></td>
    <td width="50%"><img src="https://raw.githubusercontent.com/D0n9X1n/mux-agents/main/assets/preview/mux-agents-safety.png" alt="A spawn succeeding, then a seventh spawn refused with check cap and exit 2"></td>
  </tr>
</table>

## Quick start

Install in Claude Code, Copilot CLI or both. The install carries prebuilt binaries for every platform, so there is nothing to build or download:

```sh
claude plugin marketplace add D0n9X1n/mux-agents && claude plugin install mux-agents@mux-agents
copilot plugin marketplace add D0n9X1n/mux-agents && copilot plugin install mux-agents@mux-agents
```

Start the client inside tmux or rmux, then just ask: *"spawn a Copilot agent to review the diff, a Claude agent to write the parser tests, and open the side bar"*.

```sh
tmux                                   # or: rmux
claude --dangerously-skip-permissions  # or: copilot --allow-all
```

Upgrade with one line:

```sh
claude plugin marketplace update mux-agents && claude plugin update mux-agents@mux-agents
copilot plugin marketplace update mux-agents && copilot plugin update mux-agents@mux-agents
```

## Learn more

[Usage](https://github.com/D0n9X1n/mux-agents/wiki/Usage) (commands, side bar, safety checks, upgrades, troubleshooting) · [Wiki](https://github.com/D0n9X1n/mux-agents/wiki) (English · 简体中文) · [Changelog](https://github.com/D0n9X1n/mux-agents/blob/main/CHANGELOG.md) · [Contributing](https://github.com/D0n9X1n/mux-agents/blob/main/CONTRIBUTING.md) · [Security](https://github.com/D0n9X1n/mux-agents/blob/main/SECURITY.md) · MIT
