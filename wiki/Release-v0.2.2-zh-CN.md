# mux-agents v0.2.2 发布说明

[English](https://github.com/D0n9X1n/mux-agents/wiki/Release-v0.2.2)

在 tmux 或 rmux 窗格中启动 Claude Code 和 Copilot CLI 代理，查看每个代理在做什么，给它们发消息并停止它们。

## 变化

- **一眼就能看懂的侧边栏。** 每个运行中的代理显示状态（`◉ working`、`◌ waiting`）、用各自颜色显示的名字、`[model]`，以及未读邮件 `✉ N`；已结束的代理随后显示为 `✓ done`、`✗ exit N` 或 `- stopped`。各行按窗格的实际宽度和高度排版并原地重绘，不再闪烁。遵循 `NO_COLOR`、`FORCE_COLOR` 和 `MUX_AGENTS_ASCII=1`。([#37](https://github.com/D0n9X1n/mux-agents/issues/37))
- **侧边栏固定在右边缘**，占满高度，宽 `max(40, window width / 5)` 列（窗口宽度的五分之一）；分屏、布局变化、`swap-pane` 或 `rotate-window` 之后会被放回原处。([#36](https://github.com/D0n9X1n/mux-agents/issues/36))
- **`sidebar -Close` 不会再关闭你自己的窗格。** 侧边栏是 `sidebar` 启动的那个进程，由其窗格、pid 和启动时间证明，绝不依据标签。([#47](https://github.com/D0n9X1n/mux-agents/issues/47))
- **代理按任务命名**（`add-subtract-function`），名字显示为标签页名和窗格标题，在会话内绝不重复使用。([#39](https://github.com/D0n9X1n/mux-agents/issues/39))
- **丢失的代理会自动关闭。** 标签页或窗格已消失的代理会被关闭，并向其父代理发送一条通知，同时从侧边栏移除；对已结束的代理执行 `stop` 会回复 `already`。([#38](https://github.com/D0n9X1n/mux-agents/issues/38))
- **`spawn -Model <model>`** 选择代理的模型；`list`、侧边栏和 Agents 窗格都会显示它。([#41](https://github.com/D0n9X1n/mux-agents/issues/41))
- **不再误报 `policy` 拒绝**：处于 bypass 模式的 Claude Code 调用方，其页脚在上方几行或换行时也能识别；每个 `policy` 拒绝都会说明原因。([#40](https://github.com/D0n9X1n/mux-agents/issues/40))
- **仅限终端 CLI。** Claude Code 桌面应用、网页、IDE 面板和无界面运行不会有 Agents 窗格、`/mux-agents-pane` 命令或轮询。([#43](https://github.com/D0n9X1n/mux-agents/issues/43))

## 安装

```text
claude plugin marketplace add D0n9X1n/mux-agents
claude plugin install mux-agents@mux-agents
copilot plugin marketplace add D0n9X1n/mux-agents
copilot plugin install mux-agents@mux-agents
```

## 升级

从 0.2.0 或 0.2.1 升级时，先用 `sidebar -Close` 关闭侧边栏，然后在每个客户端中用一行命令更新，再重启会话：

```text
claude plugin marketplace update mux-agents && claude plugin update mux-agents@mux-agents
copilot plugin marketplace update mux-agents && copilot plugin update mux-agents@mux-agents
```

由 0.2.1 打开且仍未关闭的侧边栏没有其进程的记录，因此 0.2.2 绝不会关闭它：`sidebar` 会拒绝并指出它；用 `kill-pane -t <pane>` 关闭。在 0.2.x 内，状态文件夹以及记录、邮件和锁文件的格式保持不变；记录新增 `model` 和 `color`，旧记录只是没有这两项。代理窗格在你重启之前继续使用它们启动时的二进制文件；如果想让它们使用 0.2.2，先 `stop` 它们。

## 已知限制

- 侧边栏的右边缘锁定已在 macOS 上的 tmux 3.8、tmux 3.2a 和 rmux 0.10 上实测；原生 Windows rmux 未经测试。
- rmux 0.10 的 `rotate-window` 会把代理标签留在错误的窗格上，因此旋转后 `list` 和 `read` 可能指向错误的窗格（[#48](https://github.com/D0n9X1n/mux-agents/issues/48)）。
- 在 Claude Code 桌面应用中确认没有 Agents 窗格的手动检查仍待完成（[#43](https://github.com/D0n9X1n/mux-agents/issues/43)）。
