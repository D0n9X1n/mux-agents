# mux-agents 百科

[English](Home)

mux-agents 是 Claude Code 和 GitHub Copilot CLI 的插件。在 tmux 或 rmux 窗格中运行的客户端可以用它在你看得见的窗格里启动一个由 Claude Code 和 Copilot CLI 代理组成的团队，让它们通过文件互相发消息，并在窗格关闭、多路复用器服务器退出或机器重启后让每个代理继续运行。
它是一个原生 Rust 二进制文件 `mux-agent`，运行在 macOS、Linux 和 Windows 上。**先阅读[使用](Usage-zh-CN)**，安装它并运行你的第一个团队。

### 用户手册

- [使用](Usage-zh-CN) — 安装、命令、安全检查、要求、升级和故障排查
- [团队与生命周期](Teams-and-Lifecycle-zh-CN) — 用 `up` 启动团队，启动、停止、恢复和修复代理，并在崩溃或重启后找回团队
- [侧边栏](Side-Bar-zh-CN) — 侧边栏显示什么、它的按键和鼠标操作，以及它如何绘制
- [消息与笔记本](Messaging-and-Notebook-zh-CN) — 代理之间的邮件、邮件如何到达各客户端，以及团队笔记本

### mux-agents 如何工作

先阅读[架构](Architecture-zh-CN)了解全貌，再阅读
[从启动到窗格](From-Spawn-to-Pane-zh-CN)了解一个代理的完整流程。
需要了解某个子系统时，使用下面的参考页面。

- [架构](Architecture-zh-CN) — 要解决的问题、各组成部分和设计规则
- [从启动到窗格](From-Spawn-to-Pane-zh-CN) — 一次 `spawn`，从请求到窗格中运行的代理
- [代理生命周期](Agent-Lifecycle-zh-CN) — 代次、停止、恢复、修复、还原，以及 `up` 的再次进入
- [状态与持久性](State-and-Durability-zh-CN) — 状态根目录、只创建一次的发布、锁，以及失败即关闭的读取
- [安全与占用](Safety-and-Occupancy-zh-CN) — 启动检查、进程身份，以及什么可能占用一段对话
- [模块参考](Module-Reference-zh-CN) — 每个源码模块及其职责

### 构建与贡献

- [开发与发布](Development-and-Release-zh-CN) — 目录结构、必须通过的检查、实时测试、合并、发布和百科发布
