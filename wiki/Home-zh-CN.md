# mux-agents 百科

[English](Home)

mux-agents 是 Claude Code 和 GitHub Copilot CLI 的插件。客户端在 rmux 或 tmux 窗格中运行时，它可以在同一会话的新窗口或窗格中启动其他 Claude Code 或 Copilot CLI 代理，并分叉上下文，然后列出、聚焦、给它们发消息和停止它们。

## 状态

v0.1.0 正在开发中。代码在 Windows 上配合 rmux 0.10.0 运行，在 Linux 和 macOS 上配合 tmux 3.2 或更高版本运行。侧边栏（#4）已进入代码：每个客户端都可用的多路复用器分屏，以及 Claude Code 中的窗格。

在 Linux 和 macOS 上，如果 .NET 早于 8.0.32、9.0.21 或 10.0.13，其他本地用户可以让 mux-agent 命令以 lock 检查超时。不会泄露内容，也不会损坏状态。请使用最新的 PowerShell。

## 设计

- [基础](Design-Foundation-zh-CN) — v0.1.0 子项目 A：改名为 mux-agents、在 Linux、macOS 和 Windows 上运行 CI、由标签驱动的发布
- [侧边栏](Design-Side-Bar-zh-CN) — v0.1.0 子项目 C：代理数量和每个代理正在做的事，显示在多路复用器分屏和 Claude Code 窗格中
