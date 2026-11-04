# 侧边栏：有多少代理，各自在做什么

[English](Design-Side-Bar)

里程碑 v0.1.0 的子项目 C，问题 #4。以 A 和 B 为基础。本页描述侧边栏现在的样子；“历史”一节说明它如何演变至此。

## 目的

在主聊天会话中显示一个侧边栏，给出代理数量，并为每个代理用一行说明它在做什么，两种客户端都支持。

## 决定（在发布目标下由 PM 做出的决定）

1. **一个数据源，两种视图。** `list -All` 是数据源。Claude Code 在聊天旁的窗格中绘制它。每个客户端（包括 Copilot CLI）都会以一个窄的多路复用器分屏得到它。
2. **“在做什么”从不抓取屏幕**（客户端的最后一行是它的页脚）。按顺序取：代理最近的 `status`、它最近发送的消息的第一行、它的任务的第一行。
3. **一个插件。** Copilot CLI 1.0.105 加载带 hooks 模块的插件时不报警告（本地探测），所以 Claude Code 窗格随 `mux-agents` 一起发布。
4. **侧边栏是一个进程，而不是一个标签。** 会话记录窗格以及 `sidebar` 启动的 `watch` 进程；`@agent-role sidebar` 只是一个标签，而 rmux 的 `rotate-window` 会把标签留在原处（#47）。只有被记录的进程才会被移动、调整大小或关闭。
5. **侧边栏守住自己的位置。** 它属于右边缘、占满高度，用户无需把它放回去；放回时不移动用户的焦点。

## 动词

- **`status (-Body <text> | -BodyFile <path>)`。** 只能从代理窗格运行。人工窗格拒绝 `target`（"only an agent reports status"）。代理必须使用 `-BodyFile`，与 `send` 相同。保留第一个非空行，去掉控制字符，截断到 80 个字符。在代理的文件夹中写入 `status.json`（`text`、`at`），并返回 `{ name, text }`。
- **`list` 为每个代理增加字段：**
  - `state`：`running`，已结束的代理则取自 `notice.json` 的 `exited`、`stopped` 或 `lost`。
  - `doing` 和 `doingFrom`（`status`、`message` 或 `task`）。
  - `age`：自 `createdAt` 起的秒数。
  - `unread`：调用者尚未读取的、来自该代理的消息数。计数不会移动调用者的读取偏移。
  - `-All` 加入本会话已结束的代理，最新的在前，最多 10 个，以及 `endedTotal`，即全部已结束代理的数量。
  - 结果增加 `running`，即存活代理的数量。
- **`sidebar [-Close]`。** 只有从代理窗格运行时才拒绝（`tagged`）；Claude Code 或 Copilot 主聊天和普通 shell 都可以运行。每个会话一个侧边栏，在会话的侧边栏锁下操作：已证明的侧边栏带 `opened: false` 返回，否则打开一个新的。`-Close` 先把会话选项 `@mux-agents-sidebar` 设为 `off`，使之后的 spawn 不会重新打开它，再只在它仍是被记录的进程时关闭它；`sidebar` 取消该选项。带有 `@agent-role sidebar` 却无法证明是侧边栏的窗格以 `1 sidebar` 拒绝并给出名字，从不被关闭。侧边栏没有 `@agent-id`，所以不计入上限，也不出现在 `list` 中。
- **`watch`。** 侧边栏的循环：每 2 秒（以及在侧边栏中按键或按下左键后立即）读取它自己窗格的尺寸，在进程内读取数据源，并按该尺寸绘制面板。读取面板失败或对其窗格的探测没有应答之后绘制的帧以 `refresh failed` 结尾。窗格消失时它以 `pane-closed` 结束，写入失败时以 `output-closed` 结束，连续 30 次探测没有应答（一分钟）后以 `probe-failed` 结束，在侧边栏中键入 Ctrl-C 时以 `interrupted` 结束；每次退出时它都会恢复终端。它只向终端绘制：标准输出被重定向时它以 `usage` 拒绝，所以它只作为侧边栏窗格自己的命令运行。只有侧边栏自己的 `watch` 维护布局；手动运行的会绘制并接收点击和按键。只有当标准输入和标准输出都是终端且 `TERM` 不是 `dumb` 时它才读取终端；否则它只绘制。在 Windows 上它读取控制台：VT 输入，关闭行编辑、回显、Ctrl-C 处理和快速编辑，在输入句柄上等待并读取按键记录（从不用 `ReadFile`，调整大小的记录会让它阻塞），并由控制台控制处理程序在 Ctrl-C、Ctrl-Break 或关闭时恢复模式。

`spawn` 把 `taskLine`（任务的第一个非空行，80 个字符）写入 `record.json`。代理的 `send` 在发送者的文件夹中写入 `last-sent.json`（`line`、`at`）。每个 `doing` 来源（状态、最后一行、任务行）在存储前都去掉控制字符和制表符，侧边栏在绘制前再次去除 C0、DEL 和 C1 控制字符，所以任何字段都不会移动光标或增加一行。

## 侧边栏

- **位置。** `sidebar`，或会话中第一次成功的 `spawn`，在调用者窗口的右边缘运行 `split-window -d -f -h`，占满高度，用二进制文件的绝对路径运行 `<exe> watch`；`-d` 让调用者的窗格保持活动。之后的 spawn 复用它；`sidebar -Close` 之后，只有 `sidebar` 会再次打开它。无法打开它的 spawn 仍然成功，并报告 `sidebarError`。在标签页模式下，侧边栏留在你发起 spawn 的窗口中。
- **宽度。** `max(@mux-agents-sidebar-min, window × @mux-agents-sidebar-percent / 100)`，默认为 `max(40, window × 20 / 100)`。两个选项都在侧边栏窗格所在处读取（窗格、窗口、会话，然后是 `set -g`），作为已在运行的查询中的格式字段读取，所以不增加调用；不是范围内整数的值使用其默认值。分屏用 `-l` 请求宽度，再用 `resize-pane` 设置一次，因为 rmux 忽略 `-l`。原因：固定宽度浪费宽窗口，只按比例则在窄窗口中无法阅读。
- **守护。** 每个周期侧边栏检查自己的窗口。当分屏、布局变化、`swap-pane` 或 `rotate-window` 移动了它时，用 `move-pane -d -f -h` 把它放回，重新设置宽度，并重新选中之前活动的窗格，所以用户的焦点从不移动。用户拖出的宽度会保留，直到窗口宽度或宽度选项改变。放大的窗口不受干扰，所以侧边栏从不取消窗格的放大。连续 3 次修复失败后它停止尝试，显示 `layout: repair paused`，直到布局改变或过去一分钟，所以它从不与无法修复的布局对抗。
- **重绘。** 每 2 秒（以及按键或按下左键后立即）按窗格的实际尺寸布局面板，从不换行：过长的文本用 `...` 截断，窗格较矮时先去掉空行，再去掉已结束的行，再去掉最早代理的“在做什么”行和整行；标题始终保留。每一帧在同步输出（`ESC[?2026h` … `ESC[?2026l`）中原地写入：光标归位、逐行写入、清除到行尾、清除下方。没有整屏清除，所以侧边栏不闪烁。`TERM=dumb` 时改为打印普通帧。
- **外观。** 存活的代理显示 `◉ working`（绿色）或 `◌ waiting`（青色）；最新的 3 个非 `lost` 结束的已结束代理显示 `✓ done`、`✗ exit N`（红色）或 `- stopped`（黄色），然后是 `+K more`（`lost` 的代理计入数量，从不绘制）。只有当所有已结束代理都在 `list -All` 列出的 10 个之中时，提示才提到 `list -All`，否则写 `+K more ended`，所以侧边栏从不承诺 `list -All` 不会列出的行。每个存活代理的名字有自己的颜色，取本会话存活代理用得最少的颜色，且绝不是状态颜色，所以名字和状态从不混淆。颜色遵循 `NO_COLOR`（关）、`FORCE_COLOR` 或 `CLICOLOR_FORCE`（开），然后是 `TERM=dumb`（关）；`MUX_AGENTS_ASCII=1` 为没有这些符号的终端绘制 ASCII 字形。
- **身份（#47）。** 分屏的进程必须是本二进制文件的 `watch`；只有这样，窗格才会被加上标签，并以 `<pane> <pid> <start>` 记录在会话选项 `@mux-agents-sidebar-id` 中。之后的每一步都要证明被记录的窗格仍存活且仍是那个进程。带标签却不是它的窗格会被拒绝并给出名字，从不被关闭：它可能是旧的侧边栏，也可能是用户自己的、被 rmux 移来标签的窗格。

## Claude Code 窗格

`hooks/hooks.json` 指定 `hooks/register.tsx`，`plugin.json` 指定声明窗格状态的 `types/index.d.ts`。该窗格与多路复用器侧边栏相互独立：`sidebar -Close` 不会关闭它。

- **打开时机：** 一次运行入口的 `spawn`、不是错误且输出含成功 spawn 回复（JSON 或 `-Text`）的 `Bash` 或 `PowerShell` `tool.call` 之后，且会话是 tmux 或 rmux 中的终端 CLI 会话时：`$.ui.open({ id: 'mux-agents', title: 'Agents' })`。`/mux-agents-pane` 可按需打开它（`/mux-agents` 归技能所有）。子代理从不 spawn，所以只有发起会话会打开它。
- **刷新：** `$.clock.every(3000)` 把入口的 `list -All` 的结果写入一个 atom，并设置 `$.ui.status('agents: <running>')`。一次刷新只在上一次结束后开始；每次运行 10 秒超时。
- **绘制：** 计数行，然后每个代理的名称、客户端、`[model]`、暗色的 `doing` 和存活时长；已结束的代理变暗；刷新失败时保留上一次的面板，并标注 `refresh failed`。绘制的每个字符串都去掉 C0、DEL 和 C1 控制字符，制表符变成空格。

## 技能

`SKILL.md` 写明：代理开始一个步骤时运行 `status -BodyFile <file>`；第一次 `spawn` 打开侧边栏，`sidebar -Close` 在本会话中关闭它，`sidebar` 再次打开它；`list -All` 加入已结束的代理（最多 10 个）。

## 组件图

| 组件 | 职责 | 接口 | 复用或扩展 | 可能的变更 | 测试接缝 |
|---|---|---|---|---|---|
| `agents::view` | 存储代理的状态；`list` | `status`、`list` 动词 | `mail::doing_line`、代理锁 | 更多字段 | 替身提供程序 |
| `agents::board` | 数据源 | `{running, endedTotal, agents}` | 记录、通知、邮箱 | 列 | 替身提供程序 |
| `sidebar` `layout` / `frame` | 侧边栏的行和帧 | 面板 → 行 → 文本 | `paint` | 布局 | 纯单元测试 |
| `sidebar` 打开、守护、`watch` | 打开、查找、关闭侧边栏；守住位置；循环 | 动词 | `mux::native`、会话选项 | 尺寸 | 替身多路复用器脚本；真实 tmux 和 rmux |
| `paint` | 颜色开关、颜色、显示列宽、截断 | 函数 | `unicode-width` | 字形 | 单元测试 |
| `hooks/register.tsx` | Claude Code 窗格 | `/mux-agents-pane`；spawn 时自动打开 | 插件 mod API | 绘制 | `claude plugin test` |

## 测试

- **单元测试（所有操作系统，替身提供程序）。**
  - `status`：截断、控制字符、从人工窗格运行时拒绝、代理使用 `-Body` 时拒绝。
  - `doing` 顺序：先状态，再消息，再任务。
  - `unread` 计数且不移动偏移。
  - `-All` 覆盖每种结束类型，上限 10 个，`endedTotal` 统计全部。
  - `sidebar`：每个会话一个侧边栏、`-l` 重试、`-Close`、从代理窗格运行时拒绝；只有存活的、被记录的 `watch` 才能证明侧边栏；无法证明的标签导致拒绝，且不关闭任何窗格。
  - 在多种尺寸下对固定面板布局，包括 emoji 和 ZWJ 序列，以及字段中含 ESC 序列、BEL 和 C1 字节的面板：输出中不含任何这些字符。
  - 守护：被移动的侧边栏回到原位且焦点不变，放大的窗口不受干扰，3 次失败暂停修复，窗口或选项改变时调整大小。
- **真实环境测试（Linux、macOS）。** 真实的 tmux 3.8、tmux 3.2a 和 rmux 0.10.0 服务器：侧边栏在右边缘按其宽度打开，绘制面板，布局被破坏后回到原位，`-Close` 关闭它。
- **Windows 控制台（#70）。** 测试二进制文件以自己的隐藏控制台再次运行自身：设置进入原始模式，写入的记录按其字节读回，单独的调整大小记录不会提前结束等待，Drop 恢复模式，设置之后或之中的 Ctrl-Break 都会使模式恢复。
- **真实环境测试（Windows）。** `tests/live_windows.rs` 在私有的 rmux 0.10.0 服务器中运行（CI 中使用调试构建，因为发布版守护进程无法在运行器的作业对象中启动，rmux#238）：在侧边栏中按 `j` 再按 Enter 以及点击都会切换到代理，Ctrl-C 结束手动运行的 `watch` 并关闭鼠标报告。
- **Claude Code 窗格。** `claude plugin test` 下的 `tests/claude/register.test.tsx` 和 `tests/claude/board.test.ts`：一次 spawn 工具调用打开窗格，桩化的 `list` 渲染计数和各行，刷新失败时保留上一次的面板。lint 作业安装 Claude Code 2.1.310，并运行 `claude plugin validate .` 和 `claude plugin test .`。

## 必过项

在所有操作系统上运行 `cargo test --features test-panic --locked`，在 Linux 和 macOS 上运行 `cargo test --features live,test-panic --locked --test live`，在 Windows 上运行 `cargo test --features live,test-panic --locked --test live_windows`，以及 `claude plugin test .`。

## 非目标

从对话记录推断；`pane-border-format`；Copilot CLI 面板；通知。

## 先例

| 做法 | 来源 | 与我们约束的契合度 | 证据 |
|---|---|---|---|
| 用窗格用户选项携带代理状态 | ntm `@ntm_agent_mail_state` | 与 `@agent-role` 和 `status` 作用相同 | 调研 |
| 状态来自代理写入的文件 | Claude Code 代理团队收件箱 | 相同：`status.json`、邮箱 | 调研 |

## 历史

- **0.1.0。** `sidebar` 在调用者旁打开 `split-window -d -h -l 42`，运行 PowerShell 入口的 `watch`，并按 `@agent-role sidebar` 标签查找它。`watch` 每 2 秒清屏并绘制 `mux-agents · 3 running`，然后是 `● claude-1  claude  4m` 这样的行。在 Copilot CLI 中，你要在第一次 `spawn` 之后自己运行 `sidebar`。
- **0.2.1。** 第一次成功的 `spawn` 打开侧边栏；`sidebar -Close` 在本会话中关闭它（#35）。
- **0.2.2。** 侧边栏按进程而不是标签查找（#47、#50），守在右边缘、占满高度，宽 `max(40, window / 5)` 列；按状态和代理着色、`NO_COLOR`、ASCII 字形，以及用同步输出原地重绘（#51）。
- **0.2.4。** 宽度为 `max(@mux-agents-sidebar-min, window × @mux-agents-sidebar-percent / 100)`，默认 30 列和 5%（#59）。
- **0.2.5。** 左键点击代理的名字行，或在侧边栏中用 `j`/`k` 和 Enter，即切换到该代理的窗口和窗格；代理文本中的 `◉`/`◌` 绘制为 `•`（#61）。
- **0.2.6。** 默认宽度恢复为窗口的 20%，至少 40 列（#74）。Windows 上也支持点击和按键：`watch` 以 VT 输入模式读取控制台，并在每次退出时恢复它（#70）。

## Rust 实现（0.2.0）

0.2.0 保留数据源、窗格和分屏；侧边栏和代理窗格直接运行二进制文件。

- **入口。** `scripts/mux-agent`（POSIX sh）或 `scripts\mux-agent.cmd` 依次尝试 `MUX_AGENTS_BIN`、`scripts/bin/<os>-<arch>/mux-agent[.exe]` 和 `target/release/mux-agent[.exe]`，运行第一个可用的；都不可用时打印 JSON `entry` 拒绝并以 1 退出。
- **锁。** 在 Unix 上，锁是 passwd 主目录下 `.local/state/mux-agents/locks` 中的文件，用 `File::try_lock` 获取；Windows 使用命名互斥体 `Local\MuxAgents-<hex>`。锁按用户划分，同一用户的所有状态根共用它们，`GHSA-2j93-m82w-73j2` 不再适用。
- **进程层。** 不启动辅助进程：Linux 读取 `/proc` 并通过 `pidfd` 结束进程；macOS 使用 `proc_pidinfo`、`KERN_PROCARGS2` 和 `sandbox_check`；Windows 使用 Toolhelp、`GetProcessTimes`、`ProcessCommandLineInformation`、进程令牌和 DACL。
- **交付。** `main` 只有源码。标签 `v<version>` 把六个二进制文件构建到 `scripts/bin/`，附带一个 `SHA256SUMS`，并提交到 `.claude-plugin/marketplace.json` 固定的 `dist` 分支；`dist` 只会移到更新的版本。
- **窗格。** 代理窗格运行 `<exe> launcher <spec.json>`，侧边栏运行 `<exe> watch`，都使用二进制文件的绝对路径，所以窗格一直使用它启动时的二进制文件。Claude Code 窗格运行 `sh <root>/scripts/mux-agent list -All`，当 `OS` 为 `Windows_NT` 时运行 `cmd.exe /d /c <root>\scripts\mux-agent.cmd list -All`；遇到运行 `mux-agent.ps1 … spawn` 的 0.1 命令行时仍会打开。
