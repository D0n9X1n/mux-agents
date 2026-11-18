# 安全与占用

[English](Safety-and-Occupancy)

mux-agents 让智能体在没有人逐步看着的情况下启动、发送消息给、停止其他智能体。本页列出防止它造成损害的规则：谁可以启动智能体、带什么权限，如何证明一个进程是成员，恢复一个已停止的对话之前什么必须成立，以及谁可以对谁做什么。每条规则读取的都是[状态与持久性](State-and-Durability-zh-CN)中描述的状态，并且每条规则都失败即关闭：证据无法读取时，答案是安全的那个，绝不是方便的那个。

## 威胁模型

范围之内：

- **崩溃和断电**，发生在启动、停止或 `up` 的任何一步。每一步都是一次持久写入加一个崩溃点，下一个命令会结算剩下的状态。
- **重启和服务器重启。** 服务器由其套接字、pid 和启动时间命名，因此重启后的服务器是一个新键。Linux 的启动时间从开机算起，因此实时会话登记表记录开机 id，来自另一次开机的条目视为已死（`src/state/live.rs`）。
- **撕裂或异常的文件。** 撕裂的 JSON 文件、FIFO、链接，或本应是文件夹的地方是文件，都读作 `Unreadable`，而 `Unreadable` 从不算作空闲或缺失。
- **其他代。** 每次启动都是一个新的代，带有自己只创建一次的证据，每次写入都受其所属的代守卫，因此旧的启动无法覆盖新的启动。
- **PID 复用。** 进程由其 pid 和启动时间命名，从不只用 pid。遍历在启动时间晚于其子进程的父进程处结束；读取一行时最后再读一次启动时间；杀死进程通过在读取启动时间之前打开的句柄进行，因此无法波及被复用的 pid（`src/proc/seam.rs`）。
- **机器上的其他用户。** 状态文件夹仅对该用户私有，锁按用户划分（[状态与持久性](State-and-Durability-zh-CN)）。

范围之外：以同一用户身份运行的恶意进程。它本就能读写该用户的文件、向该用户的进程发信号，因此 mux-agents 内部的任何检查都无法阻止它。这里的规则防范的是意外、竞争和其他用户。

## 启动检查

每次拒绝都给出它的检查名（`code`、`check`、`message`）；面向用户的表格见[使用](Usage-zh-CN)。

| 检查 | 拒绝 | 原因 |
|---|---|---|
| `sandbox` | 调用方运行在沙箱中：`SANDBOX_RUNTIME=1`；Linux 的 `NoNewPrivs` 已设置或无法读取；macOS 的 `sandbox_check` 非零；Windows 令牌无法读取、属于 AppContainer、受限或低于 Medium 完整性级别 | 启动的智能体运行在新的多路复用器终端中，而不在调用方的沙箱里，否则沙箱中的调用方就能借此到达沙箱之外 |
| `tagged` | 团队之外，窗格带有智能体标记或本身是某个智能体窗格的调用方；成员证明失败的 herdr 终端 | 团队之外只有顶层对话能启动智能体，因此智能体不能在没有树来约束的情况下扩散；证明失败绝不回退到窗格身份 |
| `flags` | 从智能体往上直到窗格的任何 `claude` 或 `copilot` 进程带有允许列表之外的参数，或其命令行无法读取 | 策略和会话检查只理解允许列表中的参数；它们无法判断的参数或看不到的命令行会被拒绝，而不是猜测 |
| `policy` | 调用方当前的策略不是完全绕过 | 子智能体以完全权限启动，因此只有本身已拥有这些权限的调用方才能启动它 |
| `directory` | 调用方的启动目录无法确认，或不是已存在的绝对路径文件夹 | 子智能体在调用方启动的位置启动；相对路径会相对 `mux-agent` 自己的工作目录解析，而不是智能体的 |
| `cap` | 团队之外，会话中已有 6 个存活的智能体 | 约束没有树的对话 |
| `depth` | 团队之内，子智能体深度超过 3（负责人为 0） | 约束团队能扩散多深 |
| `limit` | 团队之内，除负责人外已有 6 名成员；被持有的生命周期前文件夹也计入 | 约束团队的规模，无法分类的文件夹保留其名额 |
| `ceiling` | 团队之内，子智能体的参数高于其父智能体的策略或团队的权限上限 | 子智能体获得的权限绝不超过其父智能体或团队 |
| `budget` | 团队之内，团队的令牌桶已空时成员发起的启动 | 约束成员启动智能体的速度 |

各检查在哪里运行：

- `sandbox` 在每个公开命令中、任何多路复用器调用之前运行（`src/cli.rs`、`src/sandbox.rs`），不只在 `spawn` 中。mux-agents 准入的启动器窗格在被启动时已经通过它，因此不再读取任何东西。
- 团队之外，未被准入的智能体调用方依次通过 `flags`、`policy` 和 `directory`（`src/checks.rs` 中的 `checks::assert_spawn_allowed`），然后是 `cap` 和名称检查（`src/agents/membership.rs`）。
- 团队之内，`depth` 在子智能体被准入时检查，然后是 `limit`、`ceiling`、名称，以及成员调用方的 `budget`，全部在团队锁下进行（`src/agents/tree.rs`）。预算是 `teams/<sha16>.budget.json` 中的令牌桶，由会话选项 `@mux-agents-spawn-budget` 和 `@mux-agents-spawn-refill` 设置（`src/agents/budget.rs`）；启动在 `hc run` 之前的最后一步才取走令牌，因此之后的拒绝不会花掉它。

`policy` 和 `directory` 如何读取调用方：

- **Claude Code 策略：** 会话记录中最新记录的 `permission-mode`，然后是窗格最后 8 个非空行中最下面的模式行，它必须是徽标 `⏵⏵ bypass permissions on`。窗格能捕捉到会话记录尚未记录的 Shift+Tab。缺失的模式会再读，最多 3 次，间隔 250 ms；拒绝的 `reason` 指出失败的是哪一半。
- **Copilot CLI 策略：** 最新的权限事件，没有时则看启动时的全部允许参数，并且绝不能是计划模式。
- **目录：** 在 Windows 上，Claude Code 的 `~/.claude/sessions/<pid>.json` 必须带有智能体的启动时间和本会话的 id；在 Unix 上，在两次读取智能体启动时间之间读取其内核 cwd，若会话文件存在则必须与之一致；Copilot CLI 使用其 `workspace.yaml` 中第一行 `cwd: `。

## 身份

一个进程是成员，靠的是它无法凭名字伪造的证据：它的 pid 和启动时间、它的 hc 终端地址和标签，以及它的窗格所启动的确切命令。名字只用于在记录中查找 id。

**窗格调用方**（`src/caller.rs` 中的 `caller::resolve`）。多路复用器探测给出窗格的 pid、会话、窗口和 `@agent-id`，它报告的服务器 pid 必须与端点的一致。服务器的启动时间通过打开的句柄读取，`<serverKey>` 对套接字、pid 和启动时间做哈希。然后从调用方自己的进程开始往上逐行遍历，最多 64 个父进程，在窗格处结束。启动时间晚于其子进程的父进程是被复用的 pid，遍历就此结束。调用方之上最近的 Claude Code 或 Copilot CLI 进程就是智能体，两者之间只能运行 shell（否则 2 `caller`）。智能体的启动时间通过打开的句柄读取；无法读取或已变化的启动时间拒绝为 1 `caller`。

**hc 成员**（`src/caller_hc.rs`）。当设置了 `HC_TERMINAL_ID` 且 `TMUX` 和 `RMUX` 都未设置时走这条路径。终端的 `mux.team` 标签必须是 `<16 hex>:$<n>`，其 `mux.id` 必须是智能体 id。每个文件都通过 `within_root` 读取，只有当其规范路径正是状态根目录自己的路径时才读，因此没有链接能通向外面。成员的活动代必须已提交，其记录必须通过 `Identity::read`，其 `hcAddress` 必须等于 `HC_TERMINAL_ID`，终端必须仍显示其启动（`tree::launch_holds`），且 `server.json` 必须哈希到该键。然后 `tree::prove_member` 检查进程链：调用方，之后只有 shell，然后是 `launcher.json` 记录的客户端（在 Unix 上也可以是父进程为该客户端的原生 `copilot`），其父进程是记录的启动器；每个 pid 和启动时间都必须匹配，且终端必须在前台显示其中之一。hc 只在采样过终端之后才给出其前台进程，因此 `tree::sampled` 在证明做出判断之前最多再读 `SAMPLE_READS`（20）次。

证明失败的成员是 `candidate`，绝不是 `member`。除树命令之外的每个命令都以 2 `tagged` 拒绝它；树命令会在团队锁下再次证明它（`tree::standing`），那里失败同样是 2 `tagged`，绝不回退到窗格身份。根本不是 mux 成员的 herdr 终端拒绝为 2 `herdr`。

**身份值**（`src/agents/ident.rs`）。hc 名称是 `m` 加 8 个小写十六进制位，hc 地址非空且不含空白或控制字符，成员 id 是 12 个小写十六进制位。`ident::same` 只有在两边都存在、都有效且相等时才匹配，因此两个空值或缺失值绝不匹配。未通过校验的已记录身份拒绝为 1 `evidence`，且不做任何事。

**视图**（`src/agents/core_identity.rs`）。只有当窗格的启动命令恰好是针对该成员地址的 `<program> attach <address>`，且程序是 hc 的安装名称时，该窗格才是该成员的视图（`core_identity::attach_program`）。`send`、`read`、`focus` 和 `stop` 按窗格和这条确切的命令查找智能体，从不按 `@agent-*` 标记查找，因此会把标记留在原处的 rmux `rotate-window` 无法让它们指向错误的窗格。

## 占用

`resume` 在已有的对话上启动客户端。如果另一个存活的客户端仍在写这个对话，两个客户端就会写同一份会话记录。因此在恢复之前，`occupancy::held` 会询问是否有任何存活进程可能持有该对话（`src/agents/occupancy.rs`）。每个答案都给出它的规则：

| 规则 | 何时持有 |
|---|---|
| `Table` | 进程表无法读取 |
| `Hidden` | 在 Linux 上，`/proc` 可能隐藏其他用户的进程：`/proc` 上不止一个挂载、挂载的不是 `proc`、`hidepid` 不是 `0` 或 `off`，或者带有 `subset=pid` |
| `Evidence` | 成员的记录、状态、规格或登记条目无法读取，或进程的报告或身份无法解析 |
| `Managed` | 由某个成员的代启动的客户端，其有效会话就是这个对话 |
| `Argv` | 命令行指向这个对话 |
| `Caller` | 调用方自己的客户端，其最新的登记条目指向这个对话 |
| `Unmanaged` | 本团队没有启动的同类客户端，其自己最新的报告指向这个对话 |
| `Unbound` | 无法读取、可能是客户端的进程 |
| `Reserved` | 尚未结束的代保留其对话，且没有存活的客户端绑定到它 |

当命令行中的 uuid 跟在 `--session-id` 之后，或跟在没有 `--fork-session` 的 `--resume` 或 `-r` 之后时，命令行就指向该对话。Claude Code 的分叉只占用其新对话；`--` 之后的词不是参数（`occupancy_table::occupies`）。

顺序（`occupancy::held_on`）：`/proc` 挂载、一次进程表读取、状态根目录下每个服务器键中每个成员的声明、调用方自己的客户端，然后依次检查进程表的每一行。自进程表读取以来已消失的受管客户端排在最后，保留只在没有其他持有者时才生效。

什么能清除一个进程：

- **已退出或未回收的进程不持有任何东西。** 已退出但尚未被回收的客户端仍会被列出，只是路径和 argv 已消失，因此刚 `stop` 之后它会读作 `Unbound`。`Os::ended` 回答它的情况（`src/proc/mod.rs`）：Linux 针对同一启动时间读取 `/proc/<pid>/stat`，看到僵尸进程或正在退出的进程；macOS 读取 `kinfo_proc`，因为 libproc 对僵尸进程什么都不回答；在 Windows 上已退出的进程会离开进程表。它不再运行任何代码，因此被清除。
- **本团队没有启动的客户端只凭它自己的证据持有。** 机器上打开的另一个 Claude Code 或 Copilot CLI 会话不能阻塞每一次恢复，因此同类的非受管客户端只在它自己最新的报告指向这个对话，或该报告无法读取时才持有（`occupancy_caller::unmanaged_report`）。指向另一个对话的报告不持有任何东西，自进程表读取以来已退出的客户端也一样。已知的局限是没有本插件钩子、手动切换到该对话的客户端：它看不到。
- **映像名不是该类客户端时，可清除无法读取的行。** 可执行文件在运行中被磁盘上替换、因而没有路径的 Unix `node` 按其脚本判断，因此无关的 `node` 不会持有每个 Copilot CLI 对话。
- **受管客户端的有效会话** 是它最新的报告，前提是该报告的 `at` 不早于 `launcher.json` 的 `startedAt`；更早的报告来自另一次开机，会被忽略，此时由该代自己的会话持有。
- **没有 `spec.json` 的孤儿代** 从未有过客户端，不保留任何东西。

什么会失败即关闭：无法读取的进程表或可疑的 `/proc`、无法读取的成员、列出但没有启动时间的调用方自己的 pid、有效会话未知的受管客户端，以及无法读取的非受管报告，都会持有。持有者被写为 `process <pid> (<image>)`、`member <id>` 或 `this machine`，附上规则的说明和“if you just stopped it, retry in a few seconds”。`resume` 以该消息拒绝为 2 `session`，恢复拒绝为 2 `running`，`up` 对负责人回答 `"held": "occupied"` 并附带 `holder`。

## 调用方权限

| 操作 | 谁可以 |
|---|---|
| 团队之内的 `stop`、`read <name>`、`resume` | 人；成员只有当其 id 在目标直到负责人的祖先链中时才可以（否则 2 `authority`） |
| 团队之外的 `stop` | 人；智能体只有当它是目标记录中的父智能体时才可以（否则 2 `parent`） |
| 团队之外的 `resume` | 只有人（智能体为 2 `tagged`） |
| `spawn` | 团队之外是顶层对话；团队之内是人或成员，沿树向下 |
| 侧边栏、`placement`、通知 | 人；团队之内负责人也可以设置 `placement` |

在团队之内，`tree::with_standing` 在权限检查和操作期间一直持有团队锁，因此树在两者之间无法变化（`src/agents/tree.rs`）。祖先关系只来自记录：邮件不授予任何权限。

消息投递到邮箱，从不作为按键输入（[消息与笔记本](Messaging-and-Notebook-zh-CN)）。团队 `send` 指向 `parent`、`leader`、`children`、`team`、某个成员或某个窗格，并且是团队锁下的一条持久意图（`src/agents/team_mail.rs`）。成员发送方按 `QUOTA`（一小时内 120 条意图）计费；超过后发送拒绝为 2 `quota`。将成为第 4 跳的回复拒绝为 2 `hop`，因此两个智能体无法无休止地互相回复。智能体只通过文件（`-TaskFile`、`-BriefFile`、`-BodyFile`）传递自由文本，从不放在命令行上。

## 源码索引

| 约定 | 源码 |
|---|---|
| 沙箱 | `src/sandbox.rs`、`src/cli.rs` |
| 参数、策略、目录、名称 | `src/checks.rs` |
| 会话上限 | `src/agents/membership.rs` |
| 树：深度、规模上限、权限上限、权限、成员证明 | `src/agents/tree.rs` |
| 启动预算 | `src/agents/budget.rs` |
| 窗格调用方 | `src/caller.rs` |
| hc 成员调用方 | `src/caller_hc.rs`、`src/caller_hc_tests.rs` |
| 身份校验、视图 | `src/agents/ident.rs`、`src/agents/core_identity.rs` |
| 占用 | `src/agents/occupancy.rs`、`src/agents/occupancy_table.rs`、`src/agents/occupancy_caller.rs`、`src/agents/occupancy_generations.rs` |
| 进程表、已结束的进程、杀死进程 | `src/proc/mod.rs`、`src/proc/seam.rs`、`src/proc/linux.rs`、`src/proc/macos.rs`、`src/proc/windows.rs` |
| 团队之外的停止 | `src/agents/message.rs` |
| 团队消息 | `src/agents/team_mail.rs` |
