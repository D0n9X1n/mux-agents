# mux-agents v0.2.0 发布说明

[English](https://github.com/D0n9X1n/mux-agents/wiki/Release-v0.2.0)

在 tmux 或 rmux 窗格中启动 Claude Code 和 Copilot CLI 代理，查看每个代理在做什么，给它们发消息并停止它们。

## 变化

- mux-agents 现在是一个用 Rust 编写的原生二进制文件 `mux-agent`。任何命令都不再启动 PowerShell 或 .NET 运行时。
- 有六个存活代理时，`list` 平均约 23 ms（`scripts/dev/bench-list.sh`，Apple 芯片 Mac 上的 tmux 3.8）。
- 动词、参数、JSON 回复、状态文件夹和安全检查都与 0.1 相同。
- 入口是 `scripts/mux-agent`，在 Windows 上是 `scripts\mux-agent.cmd`，取代 `scripts/mux-agent.ps1`。
- Unix 上的锁是 `~/.local/state/mux-agents/locks` 下的文件，所以 .NET 命名互斥体公告 [GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2) 不再适用。Windows 使用命名互斥体 `Local\MuxAgents-<hex>`。

## 平台

- macOS、Linux 和 Windows，各自支持 x86_64 和 aarch64。
- 任一平台上的 rmux 0.10 或更高版本，或 macOS 和 Linux 上的 tmux 3.2 或更高版本。
- `PATH` 上有 Claude Code 或 GitHub Copilot CLI；不需要 PowerShell 或 .NET。

## 安装

```text
claude plugin marketplace add D0n9X1n/mux-agents
claude plugin install mux-agents@mux-agents
copilot plugin marketplace add D0n9X1n/mux-agents
copilot plugin install mux-agents@mux-agents
```

市场安装 `dist` 分支，它由本次发布创建：标签下的文件树，加上 `scripts/bin/` 中的六个二进制文件及其 `SHA256SUMS`。

## 已知限制

不支持在同一个状态文件夹上同时运行 0.1 和 0.2.0：0.1 的命名互斥体和 0.2.0 的锁文件不会互相串行化。
