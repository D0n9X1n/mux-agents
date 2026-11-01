# v0.1.0 的基础：改名、CI、发布

[English](Design-Foundation)

里程碑 v0.1.0 的子项目 A：#3 改名、#6 `parentAgentId`、#2 CI、#5 发布。子项目 B（#1）、C（#4）和 D（#7–#9）以它为基础。

## 目的

在有人安装之前定下名字，并参照 `D0n9X1n/SonicTerm`，让之后的每个拉取请求都在 Linux、macOS 和 Windows 上运行 CI，并提供由标签驱动的发布。

## 决定

1. 干净地改名为 `mux-agents`，不保留别名：尚未发布过任何版本。
2. 测试迁移到 Pester 4.10.1。3.4.0 在 Linux 和 macOS 运行器上无法导入（CI 探测，2026-10-30）；4.10.1 无需修改即可运行现有测试套件（Windows 上 179 通过，0 失败）。
3. 在子项目 B 之前，现有测试套件带 Pester 标签 `windows`，在 Windows 以外的系统上排除。新测试套件在所有系统上运行。
4. 一次发布是一个附带插件 zip 的 GitHub release，仅当标签位于 `main` 上、有成功的 `CI` push 运行且 `plugin.json` 版本匹配时才发布。
5. 添加 MIT `LICENSE`（PM 的决定；仓库保持私有期间可以撤回）。

## 变更

**改名（#3）。** 文件：`scripts/mux-agent.ps1`、`scripts/mux-agent-launcher.ps1`、`scripts/MuxAgents.ps1`、`skills/mux-agents/`、`tests/MuxAgents*.ps1`。每个 `RmuxAgent` 标识符都变为 `MuxAgent`，测试替身也一样（`New-FakeMuxAgentProviders`）；`rmux-agent` 文本变为 `mux-agent`；`RMUX_AGENTS_STATE` 变为 `MUX_AGENTS_STATE`；状态根目录为 `%LOCALAPPDATA%\mux-agents\state`；互斥体前缀为 `Local\MuxAgents-`；清单、`[mux-agent]` 标头、错误前缀和 README 随之更改。指代多路复用器的名字保持不变：`RMUX`、`TMUX`、`TMUX_PANE`、`rmux.exe`、`Add-FakeRmuxRule`、`-N` 规则。

**`parentAgentId`（#6）。** `Add-MuxAgentNotice` 不再读取没有任何代码写入的 `Record.parentAgentId`，而是始终投递到 `Record.parentPane` 的 `pane-<n>`。子代理不能派生代理，因此父级永远不是代理窗格。

**CI（#2）。** `.github/workflows/ci.yml`：在 `pull_request` 和 `push` 到 `main` 时运行；`contents: read`；一个并发组取消被取代的拉取请求运行；actions 按 SHA 固定并带版本注释。
- `lint`（ubuntu）：actionlint 1.7.12，按其固定的 SHA-256 校验；两个清单都能解析为 JSON。
- `test`，矩阵 `ubuntu-latest`、`macos-latest`、`windows-latest`，`fail-fast: false`：安装 Pester 4.10.1；在 Windows 上把 `TEMP`/`TMP` 设为 `RUNNER_TEMP`（8.3 形式的 `RUNNER~1` 路径会让四个测试失败，CI 探测）；运行 `tests/Invoke-MuxAgentTests.ps1`，它在 Windows 以外的系统上排除标签 `windows`，任何测试失败都以 1 退出。

**发布（#5）。** `.github/workflows/release.yml` 在标签 `v[0-9]+.[0-9]+.[0-9]+` 上运行，作业由 `needs` 串联：
- `validate`（只读）运行 `scripts/release/Test-MuxAgentRelease.ps1`，它接收标签、标签 SHA、`origin/main` 祖先判断结果、该 SHA 的 `ci.yml` push 运行以及清单版本，以 1 退出并指出第一个失败：不在 `main` 上、没有成功的 CI 运行，或版本不匹配。
- `package`（`needs: validate`，只读）：用 `git archive` 把 `.claude-plugin`、`skills`、`scripts`、`hooks`（存在时）、`README.md`、`LICENSE` 打包成 `mux-agents-<tag>.zip`，作为构件上传。
- `publish`（`needs: [validate, package]`），唯一拥有 `contents: write` 的作业：`gh release create <tag>`，附带 zip 和 `--generate-notes`，存在 `wiki/Release-<tag>.md` 时将其内容放在说明开头。

**合并流程**（本计划不设分支保护），写在 `CONTRIBUTING.md` 和 PR 模板中：读取 `gh pr view <n> --json headRefOid,statusCheckRollup`；该头 SHA 上的每个检查都必须是 `SUCCESS`，且 `lint` 和全部三个 `test` 作业都存在；然后执行 `git fetch origin <sha>` 和 `git push origin <sha>:main`（不强制），这样只有经过检查的头提交落地，而且只能以快进方式落地；随后 GitHub 将 PR 标记为已合并。提交的作者为 `D0n9X1n <D0n9x1n@outlook.com>`；GitHub 合并会改写作者（用户要求，2026-10-30）。

**卫生。** 来自 SonicTerm：issue 模板 `bug.yml` 和 `feature.yml`（版本、操作系统、多路复用器、客户端），一个 PR 模板（摘要、类型、关闭的 issue、测试计划、合并清单），以及每月更新 `github-actions` 的 `dependabot.yml`。

## 组件图

| 组件 | 职责 | 接口 | 复用或扩展 | 可能的变更 | 测试接缝 |
|---|---|---|---|---|---|
| `scripts/MuxAgents.ps1` | 库，已改名 | `MuxAgent*` 函数 | `RmuxAgents.ps1` | B 移植它 | 替身提供程序 |
| `tests/Invoke-MuxAgentTests.ps1` | CI 和开发者共用的唯一测试入口 | `pwsh -File`；退出码 0/1 | README 测试命令 | B 落地时调整标签 | CI 日志 |
| `tests/MuxAgents.Foundation.Tests.ps1` | 在每个操作系统上证明本子项目 | Pester | 入口、库、git | B 翻转 Unix 平台用例 | 所有运行器 |
| `scripts/release/Test-MuxAgentRelease.ps1` | 发布闸门判定 | 参数输入，退出码和原因输出 | SonicTerm `validate-release-tag` | 更多闸门 | 给定输入的单元测试 |
| `ci.yml` / `release.yml` | 检查；标签 → 发布 | `lint`、`test (<os>)`；推送标签 | SonicTerm 工作流 | B 添加作业 | actionlint |

## 行为及其测试

除非另行注明，都在 `MuxAgents.Foundation.Tests.ps1` 中；每一项都在必过命令中运行。

- 不带动词运行 `pwsh -File scripts/mux-agent.ps1` 会打印 JSON，其 `check` 为 `usage`，消息中提到 `mux-agent`，退出码 2（Windows）；在 Linux 和 macOS 上打印 `check` `platform`，退出码 2。
- 未设置 `MUX_AGENTS_STATE` 而设置了 `RMUX_AGENTS_STATE` 时，状态根目录为 `%LOCALAPPDATA%\mux-agents\state`；设置了 `MUX_AGENTS_STATE` 时以它为准。
- 对一条 `parentAgentId` 为有效 12 位十六进制 id、`parentPane` 为 `%7` 的记录发出的通知，落在 `mail/pane-7.jsonl` 中，而不在 `mail/<id>.jsonl` 中。
- 在 `wiki/Design-Foundation.md` 和 `wiki/Design-Foundation-zh-CN.md` 之外的已跟踪文件上运行 `git grep -n -E 'rmux-agent|RmuxAgent|RMUX_AGENTS'`，不返回任何结果。需要旧名字的测试（这次扫描、被忽略的变量）用片段拼出它，例如 `'R' + 'MUX_AGENTS_STATE'`，因此扫描没有例外。
- `Test-MuxAgentRelease.ps1`：对位于 main 上、有成功运行且版本匹配的 SHA 通过；对不在 main 上、没有运行、只有失败的运行以及版本不匹配，分别以 1 退出并给出不同的原因（每种一个测试）。
- `release.yml` 声明 `publish`，其 `needs` 包含 `validate` 和 `package`（由一个测试解析 YAML 文本来断言）。

## 必过项

`pwsh -NoProfile -File tests/Invoke-MuxAgentTests.ps1` 在本地 Windows 和全部三个 CI 作业中通过；actionlint 没有报告；每个 PR 只通过合并流程合并。

## 非目标

Unix 支持（B）；侧边栏（C）；README 重写、图标、上线（D）；Pester 5；分支保护。

## 先例

| 做法 | 来源 | 与我们约束的契合 | 证据 |
|---|---|---|---|
| 只读令牌；只有 `publish` 写入 | SonicTerm `release.yml`（2026-10-30 阅读） | 相同 | `permissions: contents: read` |
| Actions 按 SHA 固定；Dependabot 每月 | SonicTerm `ci.yml`、`dependabot.yml` | 相同 | `actions/checkout@3d3c42e… # v7.0.1` |
| 标签在 `main` 上且有成功的 CI push 运行 | SonicTerm `validate-release-tag` | 相同，做成一个有测试的脚本 | `merge-base --is-ancestor`；运行查询 |
| 在 pwsh 7 上用 Pester 4 旧版 `Should` | 本地 Pester 4.10.1 运行 | 保留 562 个断言 | 179 通过 / 0 失败 |

## Rust 实现（0.2.0）

0.2.0 用一个 Rust 二进制文件 `mux-agent` 取代 PowerShell 脚本，保留动词、状态文件夹和拒绝名称。

- **入口。** `scripts/mux-agent`（POSIX sh）或 `scripts\mux-agent.cmd` 依次尝试 `MUX_AGENTS_BIN`、`scripts/bin/<os>-<arch>/mux-agent[.exe]` 和 `target/release/mux-agent[.exe]`，运行第一个可用的；都不可用时打印 JSON `entry` 拒绝并以 1 退出。
- **锁。** 在 Unix 上，锁是 passwd 主目录下 `.local/state/mux-agents/locks` 中的文件，用 `File::try_lock` 获取；Windows 使用命名互斥体 `Local\MuxAgents-<hex>`。锁按用户划分，同一用户的所有状态根共用它们，`GHSA-2j93-m82w-73j2` 不再适用。
- **进程层。** 不启动辅助进程：Linux 读取 `/proc` 并通过 `pidfd` 结束进程；macOS 使用 `proc_pidinfo`、`KERN_PROCARGS2` 和 `sandbox_check`；Windows 使用 Toolhelp、`GetProcessTimes`、`ProcessCommandLineInformation`、进程令牌和 DACL。
- **交付。** `main` 只有源码。标签 `v<version>` 把六个二进制文件构建到 `scripts/bin/`，附带一个 `SHA256SUMS`，并提交到 `.claude-plugin/marketplace.json` 固定的 `dist` 分支；`dist` 只会移到更新的版本。
- **测试与发布。** `cargo test`、`tests/` 下的 sh 测试和 `claude plugin test .` 取代 Pester 套件；`scripts/release/gate.sh` 取代 `Test-MuxAgentRelease.ps1`，拒绝原因和顺序相同。
