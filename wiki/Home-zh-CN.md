# mux-agents 百科

[English](Home)

mux-agents 是 Claude Code 和 GitHub Copilot CLI 的插件。客户端在 rmux 或 tmux 窗格中运行时，它可以在同一会话的新窗口或窗格中启动其他 Claude Code 或 Copilot CLI 代理，并分叉上下文，然后列出、聚焦、给它们发消息和停止它们。

## 状态

v0.1.0 在 Windows 上配合 rmux 0.10.0 运行，在 Linux 和 macOS 上配合 tmux 3.2 或更高版本运行。它提供侧边栏（#4）：每个客户端都可用的多路复用器分屏，以及 Claude Code 中的窗格。

在 Linux 和 macOS 上，早于 8.0.32、9.0.21 或 10.0.13 的 .NET 在命名互斥体中存在权限提升漏洞（[GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2)），mux-agents 的 `lock` 检查就使用命名互斥体：其他本地用户可以清除或替换文件内容、放宽文件夹权限或获得更高权限，还可以让 mux-agent 命令以 `lock` 检查超时。请在 pwsh 中用 `[Environment]::Version` 检查，并使用基于已修复 .NET 的 PowerShell。

## 设计

- [基础](Design-Foundation-zh-CN) — v0.1.0 子项目 A：改名为 mux-agents、在 Linux、macOS 和 Windows 上运行 CI、由标签驱动的发布
- [跨平台](Design-Cross-Platform-zh-CN) — v0.1.0 子项目 B：Linux 和 macOS 上用 tmux，Windows 上用 rmux，检查相同
- [侧边栏](Design-Side-Bar-zh-CN) — v0.1.0 子项目 C：代理数量和每个代理正在做的事，显示在多路复用器分屏和 Claude Code 窗格中

## 发布

- [v0.1.0](Release-v0.1.0-zh-CN) — 首个发布：在 Windows、Linux 和 macOS 上提供 spawn、list、status、send、read、focus、stop 和侧边栏
