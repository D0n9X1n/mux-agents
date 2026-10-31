# 侧边栏：有多少代理，各自在做什么

[English](Design-Side-Bar)

里程碑 v0.1.0 的子项目 C，问题 #4。以 A 和 B 为基础。

## 目的

在主聊天会话中显示一个侧边栏，给出代理数量，并为每个代理用一行说明它在做什么，两种客户端都支持。

## 决定（在发布目标下由 PM 做出的决定）

1. **一个数据源，两种视图。** `list -All` 是数据源。Claude Code 在聊天旁的窗格中绘制它。每个客户端（包括 Copilot CLI）都可以把它作为一个窄的多路复用器分屏打开。
2. **“在做什么”从不抓取屏幕**（客户端的最后一行是它的页脚）。按顺序取：代理最近的 `status`、它最近发送的消息的第一行、它的任务的第一行。
3. **一个插件。** Copilot CLI 1.0.105 加载带 hooks 模块的插件时不报警告（本地探测），所以 Claude Code 窗格随 `mux-agents` 一起发布。

## 动词

- **`status (-Body <text> | -BodyFile <path>)`。** 只能从代理窗格运行。人工窗格拒绝 `target`（"only an agent reports status"）。代理必须使用 `-BodyFile`，与 `send` 相同。保留第一个非空行，去掉控制字符，截断到 80 个字符。在代理的文件夹中写入 `status.json`（`text`、`at`），并返回 `{ name, text }`。
- **`list` 为每个代理增加字段：**
  - `state`：`running`，已结束的代理则取自 `notice.json` 的 `exited`、`stopped` 或 `lost`。
  - `doing` 和 `doingFrom`（`status`、`message` 或 `task`）。
  - `age`：自 `createdAt` 起的秒数。
  - `unread`：调用者尚未读取的、来自该代理的消息数。计数不会移动调用者的读取偏移。
  - `-All` 加入本会话已结束的代理，最新的在前，最多 10 个。
  - 结果增加 `running`，即存活代理的数量。
- **`sidebar [-Close]`。** 只有从带标签的子窗格（设置了 `AgentTag`）运行时才拒绝（`tagged`）；Claude Code 或 Copilot 主聊天（未带标签的代理调用者）和普通 shell 都可以运行。在调用者旁打开 `split-window -d -h -l 42`，运行 `pwsh -NoProfile -File <entry> watch`（若 rmux 拒绝 `-l`，则去掉它重试），标签为 `@agent-role sidebar`。每个会话一个：已存在时返回它并带 `opened: false`；`-Close` 关闭它。每个候选窗格都由它自己的探测确认，无法检查的候选窗格以 `1 sidebar` 拒绝，而不是视为不存在。它没有 `@agent-id`，所以不计入上限，也不出现在 `list` 中。
- **`watch`。** 侧边栏循环：每 2 秒询问它自己的窗格，在进程内读取数据源并重绘（ANSI 归位并清屏）一行计数 `mux-agents · 3 running`，然后为每个代理绘制 `● claude-1  claude  4m` 和缩进的 `doing` 行；已结束的代理显示 `○ copilot-1  exited 0`。由纯函数 `Format-MuxAgentBoard` 生成文本。读取面板失败或对其窗格的探测没有应答之后绘制的帧以 `refresh failed` 结尾。窗格消失时它以 `pane-closed` 结束，写入失败时以 `output-closed` 结束，连续 30 次探测没有应答（一分钟）后以 `probe-failed` 结束。它只向终端绘制：标准输出被重定向时它以 `usage` 拒绝，所以它只作为侧边栏窗格自己的命令运行。

`spawn` 把 `taskLine`（任务的第一个非空行，80 个字符）写入 `record.json`。代理的 `send` 在发送者的文件夹中写入 `last-sent.json`（`line`、`at`）。每个 `doing` 来源（状态、最后一行、任务行）在存储前都经过 `ConvertTo-MuxAgentMessageText` 并去掉制表符，`Format-MuxAgentBoard` 在绘制前再次去除 C0、DEL 和 C1 控制字符。

## Claude Code 窗格

`hooks/hooks.json` 指定 `hooks/register.tsx`，`plugin.json` 指定声明窗格状态的 `types/index.d.ts`：

- **打开时机：** 一次运行 `mux-agent.ps1 spawn`、不是错误且输出含成功 spawn 回复（JSON 或 `-Text`）的 `Bash` 或 `PowerShell` `tool.call` 之后，且设置了 `TMUX` 或 `RMUX` 时：`$.ui.open({ id: 'mux-agents', title: 'Agents' })`。`/mux-agents` 可按需打开它。子代理从不 spawn，所以只有发起会话会打开它。
- **刷新：** `$.clock.every(3000)` 把 `$.process.run([pwsh, -NoProfile, -File, <plugin root>/scripts/mux-agent.ps1, list, -All])` 的结果写入一个 atom，并设置 `$.ui.status('agents: <running>')`。一次刷新只在上一次结束后开始；每次运行 10 秒超时。
- **绘制：** 计数行，然后每个代理的名称、客户端、暗色的 `doing` 和存活时长；已结束的代理变暗；刷新失败时保留上一次的面板，并标注 `refresh failed`。绘制的每个字符串都去掉 C0、DEL 和 C1 控制字符，制表符变成空格。

## 技能

`SKILL.md` 增加：代理开始一个步骤时运行 `status -BodyFile <file>`；在 Copilot CLI 中，第一次 `spawn` 之后运行一次 `sidebar`；`list -All` 显示已结束的代理。

## 组件图

| 组件 | 职责 | 接口 | 复用或扩展 | 可能的变更 | 测试接缝 |
|---|---|---|---|---|---|
| `Invoke-MuxAgentStatus` | 存储代理的状态 | `status` 动词 | `Read-MuxAgentText`、`ConvertTo-MuxAgentMessageText`、锁 | 更多字段 | 替身提供程序 |
| `Get-MuxAgentBoard` | 数据源 | 类似 `list` 的字典 | `Invoke-MuxAgentList`、通知、邮箱 | 列 | 替身提供程序 |
| `Format-MuxAgentBoard` | `watch` 的文本 | 面板 → 字符串 | 无 | 布局 | 纯单元测试 |
| `Invoke-MuxAgentSidebar` / `Watch` | 打开、查找、关闭分屏；循环 | 动词 | `Invoke-MuxAgentNative`、窗格标签 | 尺寸 | 替身 rmux 规则；tmux 集成测试 |
| `hooks/register.tsx` | Claude Code 窗格 | `/mux-agents`；spawn 时自动打开 | 插件 mod API | 绘制 | `claude plugin test` |

## 测试

- **单元测试（所有操作系统，替身提供程序）。**
  - `status`：截断、控制字符、从人工窗格运行时拒绝、代理使用 `-Body` 时拒绝。
  - `doing` 顺序：先状态，再消息，再任务。
  - `unread` 计数且不移动偏移。
  - `-All` 覆盖每种结束类型，上限 10 个。
  - `sidebar`：标签、第二次调用返回同一窗格、`-Close`、`-l` 重试；由未带标签的 Claude 调用者、未带标签的 Copilot 调用者和 shell 打开、复用和关闭；从带标签的子窗格运行时拒绝。
  - `Format-MuxAgentBoard` 作用于固定面板，包括任务行、状态和代理名称中含 ESC 序列、BEL 和 C1 字节的面板：输出中不含任何这些字符。
- **集成测试（ubuntu、macOS）。** B 的测试框架运行 `sidebar`，看到 `@agent-role` 窗格，检查对它的 `capture-pane` 显示计数和替身代理的 `status` 行，然后 `-Close`。
- **Claude Code 窗格。** `claude plugin test` 下的 `tests/claude/register.test.tsx` 和 `tests/claude/board.test.ts`：一次 spawn 工具调用打开窗格，桩化的 `list` 渲染计数和各行，刷新失败时保留上一次的面板。lint 作业安装 Claude Code 2.1.310，并运行 `claude plugin validate .` 和 `claude plugin test .`。

## 必过项

`pwsh -NoProfile -File tests/Invoke-MuxAgentTests.ps1` 和集成作业在所有运行器上通过，且 `claude plugin test .` 通过。

## 非目标

从对话记录推断；`pane-border-format`；Copilot CLI 面板；通知。

## 先例

| 做法 | 来源 | 与我们约束的契合度 | 证据 |
|---|---|---|---|
| 用窗格用户选项携带代理状态 | ntm `@ntm_agent_mail_state` | 与 `@agent-role` 和 `status` 作用相同 | 调研 |
| 状态来自代理写入的文件 | Claude Code 代理团队收件箱 | 相同：`status.json`、邮箱 | 调研 |
