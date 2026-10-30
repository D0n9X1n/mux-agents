# mux-agents 百科

[English](Home)

mux-agents 是 Claude Code 和 GitHub Copilot CLI 的插件。客户端在 rmux 窗格中运行时，它可以在同一会话的新窗口或窗格中启动其他 Claude Code 或 Copilot CLI 代理，并分叉上下文，然后列出、聚焦、给它们发消息和停止它们。

## 状态

v0.1.0 正在开发中。目前代码在 Windows 上配合 rmux 0.10.0 运行。Linux 和 macOS 上的 tmux 支持（#1）与侧边栏（#4）正在构建，都还没有进入代码。

## 设计

- [基础](Design-Foundation-zh-CN) — v0.1.0 子项目 A：改名为 mux-agents、在 Linux、macOS 和 Windows 上运行 CI、由标签驱动的发布
