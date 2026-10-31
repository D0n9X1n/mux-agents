# mux-agents v0.1.0 发布说明

[English](Release-v0.1.0)

在 tmux 或 rmux 窗格中启动 Claude Code 和 Copilot CLI 代理，查看每个代理在做什么，给它们发消息并停止它们。

## 功能

- `spawn` 在多路复用器会话的新窗口或分屏中启动一个 Claude Code 或 Copilot CLI 代理。同一客户端的代理从你的对话分叉开始；其他客户端或使用 `-NoFork` 时，从你写的简报开始。
- `list`、`focus`、`send`、`read` 和 `stop` 管理这些代理。消息通过磁盘上的邮箱传递，从不模拟按键。
- `status` 让代理说明它正在做什么。`list -All` 加入已结束的代理。
- 侧边栏显示正在运行的代理数量以及每个代理在做什么：在 Claude Code 中是聊天旁的窗格，在任何客户端中都可以用 `sidebar` 打开一个窄分屏。

## 平台

- Windows，配合 rmux 0.10.0。
- Linux 和 macOS，配合 tmux 3.2 或更高版本。
- PowerShell 7.4 或更高版本，且 `PATH` 上有 Claude Code 或 GitHub Copilot CLI。

## 安全检查

代理启动时关闭了权限提示，所以只有满足以下条件时 `spawn` 才会执行，否则拒绝：调用者未处于沙箱中（`sandbox`），已经以绕过或全部允许模式运行（`policy`），只使用允许列表中的参数（`flags`），工作目录已确认（`directory`），本身不是代理（`tagged`），且存活的代理少于 6 个（`cap`）。

## 安装

```text
claude plugin marketplace add D0n9X1n/mux-agents
claude plugin install mux-agents@mux-agents
copilot plugin marketplace add D0n9X1n/mux-agents
copilot plugin install mux-agents@mux-agents
```

发布中还附带 `mux-agents-v0.1.0.zip`，即此标签下的插件文件。

## 已知限制

在 Linux 和 macOS 上，如果 .NET 早于 8.0.32、9.0.21 或 10.0.13，其他本地用户可以让 mux-agent 命令以 `lock` 检查超时。不会泄露内容，也不会损坏状态。请使用最新的 PowerShell。

Claude Code 的 `Agents` 窗格只在 Windows 上的实际会话中检查过：`/mux-agents-pane` 能打开它并绘制面板。实际 `spawn` 之后自动打开、手动关闭，以及 Linux 和 macOS 上的窗格，由测试覆盖，未在实际会话中检查。
