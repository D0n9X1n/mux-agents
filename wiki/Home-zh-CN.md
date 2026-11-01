# mux-agents 百科

[English](Home)

mux-agents 是 Claude Code 和 GitHub Copilot CLI 的插件。客户端在 rmux 或 tmux 窗格中运行时，它可以在同一会话的新窗口或窗格中启动其他 Claude Code 或 Copilot CLI 代理，并分叉上下文，然后列出、聚焦、给它们发消息和停止它们。

## 状态

0.2.0 是一个原生 Rust 二进制文件 `mux-agent`，运行在 macOS、Linux 和 Windows 上，配合任一平台上的 rmux 0.10 或更高版本，或 macOS 和 Linux 上的 tmux 3.2 或更高版本。它不需要 PowerShell 或 .NET。它提供侧边栏（#4）：每个客户端都可用的多路复用器分屏，以及 Claude Code 中的窗格。

.NET 命名互斥体公告 [GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2) 曾影响 0.1 在 Linux 和 macOS 上的 `lock` 检查，已在 0.2.0 中修复：Unix 上的锁是 `~/.local/state/mux-agents/locks` 下的文件，Windows 使用命名互斥体 `Local\MuxAgents-<hex>`。

## 设计

- [基础](Design-Foundation-zh-CN) — v0.1.0 子项目 A：改名为 mux-agents、在 Linux、macOS 和 Windows 上运行 CI、由标签驱动的发布；其 Rust 实现（0.2.0）一节介绍二进制文件、锁和 `dist` 分支
- [跨平台](Design-Cross-Platform-zh-CN) — v0.1.0 子项目 B：Linux 和 macOS 上用 tmux，Windows 上用 rmux，检查相同；其 Rust 实现（0.2.0）一节加入了所有操作系统上的 rmux
- [侧边栏](Design-Side-Bar-zh-CN) — v0.1.0 子项目 C：代理数量和每个代理正在做的事，显示在多路复用器分屏和 Claude Code 窗格中
- [运行时检查](Design-Runtime-Check-zh-CN) — 问题 #25：在 Linux 和 macOS 上拒绝受 GHSA-2j93-m82w-73j2 影响的 .NET；0.2.0 不运行 .NET，已去掉此检查

## 发布

- [v0.2.0](Release-v0.2.0-zh-CN) — 适用于 macOS、Linux 和 Windows 的单个原生二进制文件，不需要 PowerShell；Unix 上使用文件锁
- [v0.1.0](Release-v0.1.0-zh-CN) — 首个发布：在 Windows、Linux 和 macOS 上提供 spawn、list、status、send、read、focus、stop 和侧边栏
