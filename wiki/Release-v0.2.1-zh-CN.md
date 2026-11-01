# mux-agents v0.2.1 发布说明

[English](https://github.com/D0n9X1n/mux-agents/wiki/Release-v0.2.1)

在 tmux 或 rmux 窗格中启动 Claude Code 和 Copilot CLI 代理，查看每个代理在做什么，给它们发消息并停止它们。

## 变化

- **按会话选择标签页或窗格。** `placement tab|h|v` 决定本会话剩余时间里 `spawn` 在哪里打开代理：`tab`（新窗口，默认）或 `h`/`v`（在你旁边分屏）。单独运行 `placement` 显示当前选择。它是多路复用器的会话选项，随会话结束，不写入磁盘。
- `spawn -Tab`（新增）或 `-Split h|v` 只对这一次启动覆盖会话的选择。启动回复新增 `placement` 和 `placementSource`（`flag`、`session` 或 `default`）。
- **侧边栏自动打开。** 每次成功 `spawn` 后，会找到或在你旁边打开本会话的侧边栏；每个会话仍只有一个。`sidebar -Close` 让它在本会话中保持关闭，`sidebar` 重新打开。
- 侧边栏失败从不让启动失败：代理已经启动，回复带有 `sidebar: null` 和 `sidebarError`。
- 在聊天中说：*“代理优先用标签页打开”* 或 *“代理优先放在窗格里”*。

## 安装

```text
claude plugin marketplace add D0n9X1n/mux-agents
claude plugin install mux-agents@mux-agents
copilot plugin marketplace add D0n9X1n/mux-agents
copilot plugin install mux-agents@mux-agents
```

## 升级

从 0.2.0 升级时，在每个客户端中用一行命令更新，然后重启会话：

```text
claude plugin marketplace update mux-agents && claude plugin update mux-agents@mux-agents
copilot plugin marketplace update mux-agents && copilot plugin update mux-agents@mux-agents
```

在 0.2.x 内，状态文件夹以及记录、邮件和锁文件的格式保持不变。代理窗格和侧边栏在你重启之前继续使用它们启动时的二进制文件。

## 已知限制

在标签页模式下，侧边栏留在你工作的窗口中，不会加到每个代理的标签页里。
