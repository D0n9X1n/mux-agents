# 模块参考

[English](Module-Reference)

源码树的地图：每个模块负责什么，从哪里开始读。[架构](Architecture-zh-CN)说明各部分如何组合在一起；本页给出文件名。每个模块开头都有一段 `//!` 头注释说明它的职责，本页以这些头注释为准。

## 这个 crate

mux-agents 是一个 Rust crate，名为 `mux-agent`（`Cargo.toml`），edition 2024。它从 `src/main.rs` 构建 `mux-agent` 二进制，并从 `src/bin/fake-client.rs` 构建实时测试用的替身客户端 `fake-client`，后者需要 `live` 特性。特性：`live`（实时测试、替身客户端和启动审计日志）和 `test-panic`（在指定位置 panic，供 `tests/panic.rs` 使用）。依赖：所有平台上的 `serde_json`、`sha2` 和 `unicode-width`，Unix 上的 `libc`，Windows 上的 `windows-sys`。

`src/lib.rs` 声明顶层模块；Windows、Linux 和 macOS 以外的主机会编译失败。`src/main.rs` 构建 `Ctx`，把参数交给 `hold::main`、`cli::run_launcher` 或 `cli::run`。

## 顶层模块

| 模块 | 负责 | 入口 |
|---|---|---|
| `cli` | 动词表、参数解析器和分派器 | `VERBS`、`parse`、`run`、`launcher_spec`、`run_launcher` |
| `ctx` | 每个动词运行所依赖的提供者：环境、身份、路径，以及操作系统、多路复用器、hc 和时钟接缝 | `Ctx`、`Env`、`Clock`、`SystemClock` |
| `error` | 类型化的拒绝：一个退出码、一个检查名和一条固定消息 | `Refusal`、`refuse`、`Res` |
| `json` | stdout 上的回复：一行紧凑的 ASCII JSON，或 `-Text` 的多行文本 | `to_stdout`、`to_text`、`lines` |
| `caller` | 调用者：它的窗格、服务器的身份，以及它上方最近的 Claude 或 Copilot 客户端；`caller::hc`（文件 `caller_hc.rs`）在 tmux 和 rmux 之外的 hc 终端里解析调用者 | `resolve`、`resolve_tree`、`resolve_person`、`Caller` |
| `checks` | 启动检查：智能体名字、上下文计划，以及对智能体调用者的参数、策略和目录检查 | `assert_spawn_allowed`、`resolve_name`、`context_plan` |
| `sandbox` | `sandbox` 检查：拒绝在沙箱里运行的调用者 | `assert_unsandboxed`、`decide` |
| `mux` | 多路复用器接缝：套接字和二进制都已解析好的 `<mux> -S <socket> -N <argv>`；tmux 和 rmux 的版本关卡 | `Mux`、`RealMux`、`endpoint`、`gate`、`native` |
| `hc` | herdr-core 传输层；每次调用都经过有界的进程捕获，并检查 hc 的版本 | `Hc`、`connect`、`Core`（`run`、`get`、`ls`、`kill`、`read`、`send`、`label`）、`Lazy` |
| `spawn` | 唯一启动子进程的地方：带超时的捕获调用，以及附着的客户端 | `captured`、`attached`、`attached_env`、`Out` |
| `proc` | 进程表接缝；`proc/linux.rs`、`proc/macos.rs` 和 `proc/windows.rs` 各自提供一个 `Os`，`proc/seam.rs` 负责打开和杀进程，`proc/unix.rs` 是 Linux 和 macOS 共用的部分 | `Os`、`host`、`seam::open_with`、`seam::kill_with` |
| `parse` | 对内核和命令行返回内容的纯解析器，在每种主机上都测试 | `linux_stat`、`linux_cmdline`、`procargs2`、`vnode_cwd` |
| `launcher` | 窗格启动器：等待启动提交，运行客户端一次，记录它的退出码 | `launcher`、`SCRUB` |
| `launcher_scope` | 精确的 `launcher <spec> --state-root <abs> --path <value>` 形式及其运行环境 | `argv`、`spec`、`environment` |
| `hold` | `mux-agent hold <release file>`：`up` 新建会话时的第一个窗口 | `main`、`wait` |
| `state` | 状态文件夹：根目录、私有性检查、锁和文件（见下文） | `state::root::root`、`state::lock::with_lock` |
| `sidebar` | `sidebar [-Close]` 为每个会话打开或关闭一个运行 `watch` 的侧边栏，`watch` 绘制看板直到它的窗格关闭 | `sidebar`、`watch`、`ensure`、`run_watch`、`bar_width` |
| `paint` | 侧边栏和 `list -Text` 的终端文本：颜色开关、16 种命名 ANSI 颜色、显示列宽 | `color_on`、`ascii`、`paint`、`hue` |
| `util` | 哈希、随机 id 和 `createdAt` 时间文本 | `sha256_hex`、`hex_id`、`guid`、`utc_o`、`parse_utc` |
| `testing` | 仅用于测试：`Ctx` 各接缝的记录型替身 | `Fakes`、`TempDir`、`row` |

### `state` 模块

| 模块 | 负责 | 入口 |
|---|---|---|
| `state::root` | 从环境得出状态根目录 | `root`、`HOST`、`OsKind` |
| `state::fs` | 原子 JSON 写入和有界读取；大小上限 `STATE_MAX`、`TASK_MAX`、`BRIEF_MAX`、`BODY_MAX` | `write_json`、`write_json_durable`、`read_json` |
| `state::evidence` | 代际证据和标记的唯一读取者和唯一一次性写入者 | `read`、`publish`、`unreadable` |
| `state::lock` | 按用户划分的锁：Unix 上对私有锁文件使用 `File::try_lock`，Windows 上用命名互斥体；`LOCK_WAIT` | `with_lock`、`try_with_lock`、`admission` |
| `state::live` | `live/` 下的实时会话登记表，每个客户端进程一个条目 | `report`、`lookup`、`alive` |
| `state::private_unix`、`state::private_windows`、`state::acl` | 在进程内检查状态文件夹是否只属于该用户；可能打开它们的扩展 ACL | `private_dirs`、`decide_acl`、`posix_acl_extended` |

### `sidebar` 模块

| 模块 | 负责 | 入口 |
|---|---|---|
| `sidebar::input` | 侧边栏自己的终端：`watch` 运行期间的原始模式和鼠标报告，每种退出方式都会恢复 | `open` |
| `sidebar::keys` | 把终端输入变成事件：按键和 SGR 鼠标报告 | `Decoder`、`Event` |
| `sidebar::pick` | 一次点击或按键选中什么：被点击行上的智能体，以及键盘选择 | `row_hit`、`step`、`step_live` |
| `sidebar::tree` | 团队的侧边栏呈现为一棵树，领队在最前，每个成员的子成员在它下面 | `is_team`、`walk`、`lay_team` |

## `src/agents` 目录

`src/agents/mod.rs` 存放共用的辅助函数：每个服务器的状态文件夹（`state_dir`）、锁的包装（`lock`、`try_lock`、`admit`）、锁键，以及 id、窗格和名字的校验器。下面的模块按职责分组。

### 动词与视图

| 模块 | 负责 | 入口 |
|---|---|---|
| `spawn` | `spawn`：认证调用者，连接 hc，然后在会话的启动锁下接纳并启动 | `spawn` |
| `up`（文件夹） | `up`，由人运行、为一个文件夹创建或重新进入团队的动词：发现、`up/` 下的团队索引、会话的发布、重新进入、已停止团队的退役 | `up::up`、`up::index`、`up::reenter`、`up::retire` |
| `leader` | `up` 启动的领队，深度为 0 | `prepare`、`resume_ended` |
| `view` | `list`、`focus`、`status` 和 `report` | `list`、`focus`、`status`、`report` |
| `message` | `send`、`read`、`peek`、`notify` 和 `stop`；`drop` 和 `stop -Generation` 在 `message/` 中 | `send`、`read`、`peek`、`stop_hc`、`drop_member`、`notify` |
| `resume` | `resume`：已结束的成员带着它的对话作为新成员重新开始；`resume/recovery.rs` 重试需要恢复的成员 | `resume` |
| `reconcile` | `restore`，以及 `up` 运行的那一轮：团队的每个生命周期成员都被调和，Lost 的成员作为新一代带回 | `restore`、`run`、`classify`、`observe` |
| `kept` | 保留的 shell：只有它们的 hc 标签；列出并关闭它们 | `kept`、`list`、`close` |
| `placement` | 带 `tab`、`h` 或 `v` 的 `placement`，保存为会话选项 `@mux-agents-placement` | `placement`、`read_placement` |
| `split` | 分屏视图放在哪里，让主聊天保留最大的空间 | `plan`、`even_out`、`keep_main_half` |
| `attachment` | 为成员打开一个运行 `hc attach` 的视图窗格，并清理失败的视图 | `attach`、`clean` |
| `board`、`board_rows` | 侧边栏的数据源：hc 成员关系，加上附着的视图和终端历史；每个成员一行 | `board`、`mark_seen`、`hc_row` |
| `session_report` | `report -Session`：客户端的钩子说出自己的对话；运行就绪握手 | `run`、`read_input` |

### 身份与成员关系

| 模块 | 负责 | 入口 |
|---|---|---|
| `core_identity` | 成员的 hc 身份：代和 hc 名字的形状、呈现键、如何读取视图的启动命令 | `Identity`、`is_hc_name`、`attach_program`、`Key` |
| `core_facts` | 按 `mux.*` 标签得到的团队 hc 终端的一次快照 | `take`、`Snapshot`、`Observation` |
| `membership` | 当下谁是成员：存储的记录与 hc 事实的合并、名额、名字、被取代的成员 | `Snapshot::take`、`locals`、`proven_self` |
| `member_target` | 一个名字或 id 对 `stop`、`drop` 和侧边栏指的是谁 | `named`、`StopTarget` |
| `ident` | 从证据读出的每种身份各有一个校验器；格式错误的身份从不匹配 | `hc_name`、`hc_address`、`member_id`、`same` |
| `registry` | 会话的智能体窗格、它们的记录和退出通知；条件式的窗格关闭 | `Snapshot::take`、`reap`、`reap_hc`、`add_notice`、`kill_if_dead` |
| `scope` | 调用者变成状态作用域的唯一位置：状态跟随锚点，多路复用器仍是调用者自己的 | `state`、`shown`、`team` |
| `tree` | 团队的树：调用者是否在团队里、处于哪里、它的子成员放在哪里、它可以停止谁 | `scope`、`check_depth`、`limit`、`MAX_DEPTH`、`MAX_MEMBERS` |
| `team` | 团队的记录及其权限上限，来自人自己的客户端参数 | `create`、`read`、`policy_of` |
| `ancestry` | 从一个进程走到它上方 Claude 或 Copilot 客户端的唯一路径 | `first_client`、`identity` |
| `prelife` | 记录中没有 `lifecycle: 1` 的成员：可以列出和停止，读取不超过它们的记录 | `pre_lifecycle`、`lifecycle_only`、`close_pane` |

### 启动与代

| 模块 | 负责 | 入口 |
|---|---|---|
| `core_launch` | 在团队锁下启动成员一次：spec、`launch.json`、`hc run`、地址、提交声明、晋升、视图 | `forward`、`Launch` |
| `gens` | `gens/` 下代际证据的唯一读取者 | `member_of`、`members`、`publish_once`、`committed`、`next`、`Gen` |
| `team_state` | 在团队事务内，`record.json`、`desired.json` 和 `view.json` 的唯一写入者 | `open`、`held`、`Tx`、`view` |
| `lifecycle` | 对已校验的 hc 终端事实分类，与任何视图无关 | `classify` |
| `settle` | 重放表、孤儿栅栏，以及结束一代 | `settle_gen`、`terminate`、`fence` |
| `release` | 以释放而不是杀进程的方式，结束 hc 作为恢复 shell 保留的一代 | `release`、`complete`、`finish` |
| `outcome` | 每一代唯一的结果，以及所有界面共用的同一个判断；`failure.json` 和 `outcome.json` 的唯一写入者 | `adjudicate`、`needs_recovery` |
| `options` | 可安全重放的客户端选项：把启动 argv 拆成选项、会话选择器和提示 | `classify`、`replay` |
| `copilot` | Copilot 分叉的会话副本 | `copy_session` |
| `budget` | 团队的启动预算，一个在团队锁下消耗的令牌桶 | `check`、`spend` |

### 停止、级联与收养

| 模块 | 负责 | 入口 |
|---|---|---|
| `cascade`、`cascade_journal`、`cascade_tree` | 一次停止为成员的整棵子树记录 `stopped`，关闭它，再自下而上结束；日志让下一次停止、`up` 或侧边栏刷新完成被打断的级联；会话树的一次快照 | `prepare`、`execute`、`finish`、`Tree`、`Shown` |
| `adopt`、`adopt_bar`、`adopt_route` | 成员自行结束时，最近的存活祖先收养它的子成员；侧边栏的清扫；报告路由到逻辑上的父成员 | `publish`、`apply`、`tick`、`route` |

### 消息与笔记本

| 模块 | 负责 | 入口 |
|---|---|---|
| `mail` | `mail/` 下的邮箱，每行一条 JSON 消息，通过回执游标读取 | `add_mail`、`read_mail`、`serve`、`ack`、`counts` |
| `receipt` | 邮箱回执游标，在邮箱锁下整体读取和提交；与笔记本共用 | `load`、`commit`、`settle` |
| `team_mail` | 团队消息：目标指的是谁、每次发送一个持久意图、投递到每个邮箱 | `route`、`target`、`pick`、`write_intent`、`charge` |
| `notebook` | 团队笔记本：团队文件夹下只追加的 `notebook.jsonl`；`notebook/news.rs` 写出每次启动首条消息里的笔记本段落 | `note`、`notes`、`append`、`entries` |

### 占用

| 模块 | 负责 | 入口 |
|---|---|---|
| `occupancy`、`occupancy_caller`、`occupancy_generations`、`occupancy_table` | 哪些存活进程可能占用一个对话；读不出的一律算占用。调用者自己的客户端、每个成员未结束的各代声明了什么，以及进程表中单行的规则 | `held`、`held_on`、`claims`、`occupies` |

## 测试

| 类型 | 位置 | 约定 |
|---|---|---|
| 单元 | 模块内部或旁边 | 内联的 `#[cfg(test)] mod tests`、同级的 `<module>_tests.rs` 或 `<module>/tests.rs`，各自由父模块以 `#[cfg(test)]` 声明 |
| 夹具 | `src/testing.rs`、`src/agents/mod.rs`（`fixture`）、`src/agents/*_fixture.rs` | 每个 `Ctx` 接缝的记录型替身，以及带假窗格、假客户端和私有状态根目录的 `World`；只编译进测试 |
| CLI | `tests/cli.rs`、`tests/panic.rs` | 端到端测试构建出的二进制：输入 argv，输出一行回复和一个退出码 |
| 原生 | `tests/occupancy_native.rs`、`tests/live_session.rs`、`tests/liveness_windows.rs` | 主机真实的进程表 |
| 实时 | `tests/live.rs`、`tests/live_windows.rs`、`tests/support/` | 在私有套接字上运行真实的 tmux 3.8、tmux 3.2a 和 rmux 服务器，配合 `fake-client`，需要 `live` 特性；它们启动的每个子进程都有界（`tests/support/bounded.rs`） |
| 仓库守卫 | `tests/repo.rs` | 对源码文本检查的规则 |
| Shell 与钩子 | `tests/*.sh`、`tests/shim.cmd`、`tests/claude/` | 垫片、发布、wiki 发布器和升级；Claude Code 钩子模块 |

`tests/repo.rs` 中的几个守卫：

- `process_spawn_only_in_spawn_rs`：除了 `src/spawn.rs`，没有模块启动进程。
- `state_reads_go_through_scope`：`src/agents` 和 `src/sidebar.rs` 中的状态读取都经过 `scope::state`。
- `only_team_state_writes_member_records`：只有 `src/agents/team_state.rs` 写成员文件。
- `generation_evidence_is_read_only_through_gens`：启动器和成员证明只通过 `gens` 读取证据。
- `every_test_file_is_exempt_by_name`：每个 `*_tests.rs` 和 `tests.rs` 文件都被声明为仅用于测试。
- `wiki_pairs_and_links`、`zh_twins_match_line_counts`、`no_issue_or_pull_request_numbers`：wiki 规则。

运行它们的命令见[开发与发布](Development-and-Release-zh-CN)。
