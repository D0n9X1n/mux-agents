# 跨平台：Linux 和 macOS 上用 tmux，Windows 上用 rmux

[English](Design-Cross-Platform)

里程碑 v0.1.0 的子项目 B，问题 #1。以 A 为基础。

## 目的

在 Linux 和 macOS 上以 tmux 3.2+ 运行每个动词，检查和拒绝名称与 Windows 相同。Windows 行为保持不变。

## 决定

1. **一个共享核心。** 与操作系统相关的代码位于 `$P`（`New-MuxAgentProviders`）之后：Windows 成员就是现有代码，不做改动；Unix 成员填充相同的名称；只有工厂函数分支。
2. **失败即关闭：** 无法读取的信号一律拒绝。
3. **进程身份是内核记录的启动值，精确比较。** Linux：`/proc/<pid>/stat` 中最后一个 `)` 之后的原始 `starttime` 节拍数。macOS：`Process.StartTime`（微秒）。
4. **按真实可执行文件路径识别客户端**，而不只看基本名（Claude 安装程序把 `claude` 链接到 `versions/<v>` 中）。
5. **锁和状态。** 命名互斥体保留；在 Unix 上名称为 `Global\MuxAgents-<uid>-<hash>`，因为 `Local\` 只在一个会话内有效，而每个 tmux 窗格都是自己的会话。状态：`$MUX_AGENTS_STATE`（必须是绝对路径，否则拒绝），否则为绝对路径的 `$XDG_STATE_HOME` 加 `/mux-agents/state`，否则 `~/.local/state/mux-agents/state`。每个新窗格通过 `tmux -e` 得到选定的状态根，因为窗格以服务器的环境启动。

## Unix 提供程序成员

| 成员 | Linux | macOS |
|---|---|---|
| `Processes`（pid、ppid、启动值、路径、argv） | `/proc/[0-9]*`：`stat`、`exe` 链接、按 NUL 拆分的 `cmdline` | `ps -A -o pid=,ppid=`；启动值和路径来自 `Process`；argv 来自 `sysctl KERN_PROCARGS2`（Add-Type） |
| `Open` / `Kill` | 先 `pidfd_open`，再读启动值；Kill 在该 fd 上发送 SIGKILL，它不会触及被复用的 PID，并等待不带 POLLERR 或 POLLNVAL 的 POLLIN；没有 pidfd → 不发信号，只有 `kill(pid, 0)` 返回 ESRCH 才算已退出 | 不存在与身份绑定的杀进程方式：从不杀；只有 ESRCH 才算已退出，否则报告 |
| `Sandbox`（C1） | 当 `/proc/self/status` 不可读、`NoNewPrivs: 1` 或 `SANDBOX_RUNTIME=1` 时拒绝 `sandbox` | 当 `SANDBOX_RUNTIME=1`，或 `sandbox_check(getpid(), NULL, 0)` 非零或调用失败时拒绝 |
| `Cwd`（C5，工作目录） | `/proc/<pid>/cwd` | `proc_pidinfo(PROC_PIDVNODEPATHINFO)` |
| `StateRoot` | 上述路径，经 `realpath` 规范化 | 同左 |
| `Mux` | 取 `$TMUX` 中服务器 PID 的可执行文件作为 `tmux`，否则从 `PATH` 查找；`tmux -V` 低于 3.2 时拒绝 `endpoint` | 同左 |

拒绝消息会写出信号名（"NoNewPrivs is set"）。

## 行为变更

- **端点。** Unix 上的 `$TMUX` 必须匹配 `^(/[^,\r\n]+),(\d+),(-?\d+)$`。调用仍是 `<mux> -S <socket> -N …`；探测行照旧校验。
- **调用链。** Unix 上调用者与代理之间允许的 shell：`sh`、`bash`、`zsh`、`fish`、`dash`、`ksh`、`tcsh`、`pwsh`。代理是最近的这样一个进程：真实路径为 `*/claude/versions/*` 或基本名为 `claude`、基本名为 `copilot`，或其脚本的 `realpath` 以 `@github/copilot/npm-loader.js` 结尾的 `node`（node 选项按失败即关闭读取：未知选项不识别为任何客户端）。启动值精确比较；100 ns/µs 的取整仍只用于 Windows。
- **C3** 在 Unix 上从 `Processes` 读取 argv，而不是拆分字符串。允许列表不变。
- **C5。** Unix 上的 Claude：取代理的内核 cwd，在两次相等的启动值读取之间读取；若 `~/.claude/sessions/<pid>.json` 存在，其 `cwd` 必须一致。`sessionId`/`procStart` 检查仍只用于 Windows，从而消除 `/clear` 之后的误拒（claude-code#36213）。Copilot：所有平台都用 `workspace.yaml` 的 `cwd`。
- **停止。** `kill-pane` 照旧关闭窗格及其进程组。残留进程只通过上述 Linux `pidfd` 杀死；否则保留并报告（`survivors` 附 pid），从不按 PID 发信号。
- **状态私密性。** Windows ACL 检查不变。在 Unix 上从 `/` 向下逐级检查。状态根之上每个已存在的文件夹必须属于 root 或当前用户，且除非设置了粘滞位，不得被组或其他用户写入（OpenSSH StrictModes）；在 macOS 上这些文件夹只允许 deny ACE，因为 allow ACE 优先于模式位和粘滞位。缺失的文件夹用 `mkdir(2)` 以 `0700` 创建，遇到任何已存在的条目（包括链接）都会失败，然后 `chmod 0700`。状态根及其下的每个文件夹必须：恰好为 `0700`；属于有效 uid；没有扩展 ACL（`ls -ld` 中没有 `+`，macOS 上 `ls -lde` 中没有 ACE 行，这也能发现继承的条目）；规范路径上不是链接；两次 `stat` 读取的 dev:inode 相同。`id`、`stat`、`ls` 和 `realpath` 按完全限定的 `PATH` 文件夹中的完整路径启动，从不取自当前目录。其他任何情况或检查失败，都拒绝 `state`。
- **启动器。** 在 `PATH` 上查找不带 `.exe` 的 `claude`/`copilot`，使用 `$PSHOME/pwsh`，删除父级的客户端身份变量（删除，而不是留空），不重定向地启动客户端，使其拥有窗格的终端。
- **入口。** 接受 Windows、Linux 和 macOS；否则拒绝 `platform`。

## 组件图

| 组件 | 职责 | 接口 | 复用或扩展 | 可能的变更 | 测试接缝 |
|---|---|---|---|---|---|
| `New-MuxAgentProviders` | 选择成员集 | `$P` 成员 | 现有工厂函数 | 新的操作系统 | 替身提供程序 |
| `MuxAgents.Unix.ps1` | Unix 成员 | 绑定到工厂的函数 | 无 | 客户端布局 | 测试夹具 |
| `MuxAgents.Native.Unix.cs` | `pidfd`、`sysctl`、`sandbox_check`、`proc_pidinfo` | 静态 C# 方法 | Windows `Add-Type` | 结构体布局 | Linux/macOS CI |
| `tests/integration/` | 真实 tmux | `Invoke-MuxAgentTests.ps1 -Integration` | 替身客户端 | 更多动词 | `integration (<os>)` |

## 测试

- **单元测试，所有操作系统，替身提供程序：** `comm` 含 `) (` 的 `stat`；`NoNewPrivs`、`SANDBOX_RUNTIME` 和状态不可读的拒绝；客户端身份路径；端点正则；状态根顺序；针对模式、属主、`+` ACL 和 ACE 行的状态拒绝；C5 cwd 一致与不一致；杀进程：`pidfd_open` 之后启动值不符则不发信号，最后一次检查之后被复用的 PID 从不收到信号（替身的 fd 仍绑定旧进程），macOS 和无 pidfd 时只报告不杀。
- **原生测试，ubuntu 和 macOS：** 加了 ACL 的真实文件夹（Linux 上在可用时用 `setfacl -m u:nobody:rwx`，macOS 上用 `chmod +a "everyone allow read"`）拒绝 `state`；macOS 上父文件夹带 allow ACE（`chmod +a "everyone allow add_subdirectory,delete_child"`）时，在其下创建任何东西之前就拒绝；父文件夹带可继承的 deny ACE（`chmod +a "everyone deny chown,directory_inherit"`）时，新的 `0700` 子文件夹拒绝 `state`；放在文件夹路径上的链接被拒绝，其目标的模式不变；当前目录中名为 `id` 的文件从不运行；Linux 上通过 `pidfd` 杀死一个真实子进程。
- **集成测试，ubuntu 和 macOS：** 一个私有的 `tmux -S` 服务器；`PATH` 上一个记录 argv 并等待退出文件的替身 `claude`；窗格中的 pwsh 驱动程序运行 `spawn`（窗口和分屏）、`list`、`send`、代理的 `send parent`、`read -Wait`、`read <name>`、`focus`、`stop`；断言 `@agent-*` 标签、邮箱内容、`read -Wait` 在空邮箱上等待、`stopped` 通知、没有残留进程、只在调用者中设置的状态根到达每个新窗格，以及从代理窗格 `spawn` 被拒绝为 `tagged`。
- **Windows：** 现有测试套件不变。

## 必过项

`pwsh -NoProfile -File tests/Invoke-MuxAgentTests.ps1` 在全部三个运行器上通过，新套件不排除 `windows` 标签；`integration (ubuntu-latest)` 和 `integration (macos-latest)` 通过。

## 非目标

侧边栏（C）；`CLAUDE_CONFIG_DIR`/`COPILOT_HOME`；BSD 系统；低于 3.2 的 tmux；CI 中的 rmux；Unix 上真实的客户端进程树。

## 先例

| 做法 | 来源 | 与我们约束的契合度 | 证据 |
|---|---|---|---|
| `-N` 从不启动服务器；对已消失窗格的 `display-message` 以 0 退出且字段为空 | tmux 3.2 CHANGES；tmux 3.6 探测 | 与 rmux 规则相同 | 调研 |
| 先杀，再核查残留进程 | uzi | `stop` 只通过 `pidfd` 杀残留进程 | 调研 |
| `/proc` 启动节拍、按最后一个 `)` 解析；bwrap `NoNewPrivs`、sandbox-runtime `SANDBOX_RUNTIME=1` | proc_pid_stat(5)；WSL 测试；sandbox-runtime | 身份与 C1 信号 | 调研 |

## Rust 实现（0.2.0）

0.2.0 在 Rust 中保留这些规则，并支持 macOS 和 Linux 上的 rmux。

- **入口。** `scripts/mux-agent`（POSIX sh）或 `scripts\mux-agent.cmd` 依次尝试 `MUX_AGENTS_BIN`、`scripts/bin/<os>-<arch>/mux-agent[.exe]` 和 `target/release/mux-agent[.exe]`，运行第一个可用的；都不可用时打印 JSON `entry` 拒绝并以 1 退出。
- **锁。** 在 Unix 上，锁是 passwd 主目录下 `.local/state/mux-agents/locks` 中的文件，用 `File::try_lock` 获取；Windows 使用命名互斥体 `Local\MuxAgents-<hex>`。锁按用户划分，同一用户的所有状态根共用它们，`GHSA-2j93-m82w-73j2` 不再适用。
- **进程层。** 不启动辅助进程：Linux 读取 `/proc` 并通过 `pidfd` 结束进程；macOS 使用 `proc_pidinfo`、`KERN_PROCARGS2` 和 `sandbox_check`；Windows 使用 Toolhelp、`GetProcessTimes`、`ProcessCommandLineInformation`、进程令牌和 DACL。
- **交付。** `main` 只有源码。标签 `v<version>` 把六个二进制文件构建到 `scripts/bin/`，附带一个 `SHA256SUMS`，并提交到 `.claude-plugin/marketplace.json` 固定的 `dist` 分支；`dist` 只会移到更新的版本。
- **多路复用器。** 设置了 `RMUX` 时端点取 `RMUX`，否则取 `TMUX`。服务器映像名为 rmux 或 tmux 时，二进制文件就是该映像；否则设置了 `RMUX` 时取 `PATH` 上的 `rmux`，再否则取 `tmux`。`tmux -V` 低于 3.2 或 `rmux -V` 低于 0.10 时以 `endpoint` 拒绝。每次调用都是 `<mux> -S <socket> -N …`。
