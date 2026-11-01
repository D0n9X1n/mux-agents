# mux-agents v0.2.0 release notes

[简体中文](https://github.com/D0n9X1n/mux-agents/wiki/Release-v0.2.0-zh-CN)

Start Claude Code and Copilot CLI agents in tmux or rmux panes, see what each is doing, message them and stop them.

## What changed

- mux-agents is now one native binary, `mux-agent`, written in Rust. No PowerShell or .NET runtime starts on any command.
- `list` with six live agents averages about 23 ms (`scripts/dev/bench-list.sh`, tmux 3.8 on an Apple silicon Mac).
- The verbs, their flags, the JSON replies, the state folder and the safety checks are the same as in 0.1.
- The entry is `scripts/mux-agent`, or `scripts\mux-agent.cmd` on Windows, in place of `scripts/mux-agent.ps1`.
- Unix locks are files under `~/.local/state/mux-agents/locks`, so the .NET named-mutex advisory [GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2) no longer applies. Windows uses a named mutex `Local\MuxAgents-<hex>`.

## Platforms

- macOS, Linux and Windows, each on x86_64 and aarch64.
- rmux 0.10 or later on any of them, or tmux 3.2 or later on macOS and Linux.
- Claude Code or GitHub Copilot CLI on `PATH`; no PowerShell or .NET.

## Install

```text
claude plugin marketplace add D0n9X1n/mux-agents
claude plugin install mux-agents@mux-agents
copilot plugin marketplace add D0n9X1n/mux-agents
copilot plugin install mux-agents@mux-agents
```

The marketplace installs the `dist` branch, which this release creates: the tagged tree plus the six binaries in `scripts/bin/` and their `SHA256SUMS`.

## Known limits

Running 0.1 and 0.2.0 at the same time on one state folder is not supported: the 0.1 named mutex and the 0.2.0 lock files do not serialize each other.
