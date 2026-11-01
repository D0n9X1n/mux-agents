# 运行时检查：拒绝受 GHSA-2j93-m82w-73j2 影响的 .NET

[English](Design-Runtime-Check)

## 目的

在 Linux 和 macOS 上，.NET 8.0.0–8.0.31、9.0.0–9.0.20、10.0.0–10.0.12 以及 11.0 预览版到 RC 1，会让其他本地用户通过命名互斥体和共享内存清除或替换文件内容、放宽文件夹权限或获得更高权限（[GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2)）。在这些平台上，每个动词、`watch` 和启动器都会打开命名互斥体（`Use-MuxAgentLock`）。问题 #25：拒绝这样的运行时，除非用户按名称选择接受。

## 决定（目标“address all issues”下 PM 的决定）

1. 拒绝，并保留命名互斥体。没有来源表明不使用命名互斥体的进程在受影响的运行时上是安全的，而且每个版本线都有已修复的 PowerShell（7.4.21、7.5.12、7.6.7）。
2. 覆盖开关：`MUX_AGENTS_ALLOW_AFFECTED_DOTNET` 恰好设为 `1`。此时回复带有 `advisory`。
3. 版本取自正在运行的运行时的 `AssemblyInformationalVersion`（持有 `[object]` 的程序集），绝不取 `$PSVersionTable`。
4. 不检查 Windows。
5. 已修复的运行时可能无法创建命名互斥体（没有用户数据库条目的 uid；dotnet/runtime#136236，见于 10.0.13）。创建会重试一次；第二次失败以 2 `lock` 拒绝，并指出 `XDG_RUNTIME_DIR`。
6. 合入 main，不打 v0.1.1 标签；v0.1.0 的 zip 早于此检查。

## 版本规则

`Get-MuxAgentRuntimeVerdict -Version <s>` 返回 `fixed` 或 `affected`。整个字符串必须匹配 `^(\d{1,9})\.(\d{1,9})\.(\d{1,9})(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]*)?\z`（用 `\z` 而不是 `$`，后者接受末尾的 LF）；否则视为受影响。标签缺失或为 `servicing.…` 时是 GA；`rc.N.…` 是候选版本 N（N 为数字）；`preview.…`、`alpha.…`、`beta.…` 早于 RC 1；其他标签视为受影响。已修复：次版本为 0 且为 GA，主版本 8 且补丁 ≥ 32、9 且 ≥ 21、10 且 ≥ 13；主版本 11、次版本 0、GA 或 RC N ≥ 2；任意主版本 ≥ 12。其余都视为受影响，包括低于 8 的主版本和 8–11 中非零的次版本。

## 行为

- `Get-MuxAgentRuntimeAdvisory -Platform -Version -Environment` 在 Windows 或已修复的运行时上返回 `$null`，无论是否设置覆盖开关。在受影响的 Linux 或 macOS 运行时上，它以 2 `runtime` 拒绝：`mux-agents refuses .NET <version> on <platform>: GHSA-2j93-m82w-73j2 (CVE-2026-96231). Install PowerShell 7.4.21, 7.5.12, 7.6.7 or later, or set MUX_AGENTS_ALLOW_AFFECTED_DOTNET=1 to run anyway.` 设置覆盖开关时，它返回 `.NET <version> is affected by GHSA-2j93-m82w-73j2; running because MUX_AGENTS_ALLOW_AFFECTED_DOTNET=1.`
- `Initialize-MuxAgentProviders -Entry [-Platform] [-RuntimeVersion]`（默认值：`Get-MuxAgentPlatform`、`Get-MuxAgentRuntimeVersion`）先运行公告检查，再运行 `New-MuxAgentProviders`，并设置 `$P.Advisory`。拒绝在任何提供程序、互斥体或状态文件夹之前以 `Failure` 返回；之后的构建失败带有 `advisory`。
- `Invoke-MuxAgentCommand` 在成功和失败回复中加入 `advisory`，条件是设置了 `$P.Advisory`；`-Text` 打印 `advisory: <text>`。
- 启动器在启动客户端之前把 `mux-agent: <advisory>` 写到 stderr；遇到 `Failure` 时打印回复并退出，不启动客户端（不变）。
- 每个 `watch` 画面以 `advisory: GHSA-2j93-m82w-73j2` 结尾，条件是设置了 `$P.Advisory`。
- 调用者设置了覆盖开关时，`Get-MuxAgentPaneEnvironment` 在 Unix 上加入 `-e MUX_AGENTS_ALLOW_AFFECTED_DOTNET=1`，这样获得服务器环境的新代理窗格和侧边栏窗格也能运行。
- `Use-MuxAgentLock` 只解析一次名称（那里的拒绝不重试），然后通过 `$script:MuxAgentNewMutex` 创建互斥体。只有创建时的 `IOException`、`UnauthorizedAccessException` 和 `WaitHandleCannotBeOpenedException` 会在 `& $script:MuxAgentLockSleep 100` 之后重试一次。`WaitOne`、被放弃的互斥体和操作本身不在重试之内。吸收锁失败的调用方（watch 的 `refresh failed`、启动器的通知）不变。

## 组件图

| 部分 | 文件 | 扩展 |
|---|---|---|
| 版本、判定、公告 | `scripts/MuxAgents.ps1` | `Get-MuxAgentPlatform` 旁边 |
| 检查、`Advisory` | `scripts/MuxAgents.ps1` | `Initialize-MuxAgentProviders` |
| 回复、画面、窗格 | `scripts/MuxAgents.ps1` | `Invoke-MuxAgentCommand`、`Start-MuxAgentWatch`、`Get-MuxAgentPaneEnvironment` |
| 重试接缝 | `scripts/MuxAgents.ps1` | `Use-MuxAgentLock` |
| 启动器的一行 | `scripts/mux-agent-launcher.ps1` | `Initialize-MuxAgentProviders` 之后 |
| 文档 | `README.md`、`wiki/Home*.md`、本页及其双语页 | 锁的段落 |

## 测试

- 判定，所有操作系统：`8.0.31`/`8.0.32`、`9.0.20`/`9.0.21`、`10.0.12`/`10.0.13`、`10.0.14-servicing.26522.126+0427b9a`、`11.0.0-preview.7.1`、`11.0.0-rc.1.26425.128`、`11.0.0-rc.2.26504.105`、`11.0.0-rc.10.1`、`11.0.0`、`11.0.1-servicing.1`、`12.0.0`、`7.0.20`、`10.1.0`、`10.0.13-weird.1`、`10.0.13x`、`99999999999.0.0`、`10.0.14` 后跟 LF、`''`。
- 读取器：`Get-MuxAgentRuntimeVersion` 等于实时的 `AssemblyInformationalVersionAttribute`（属于 `[object].Assembly`），其数字部分等于 `[Environment]::Version`。
- 公告：受影响时 Windows 返回 `$null`；已修复的 Linux/macOS 无论是否设置覆盖开关都返回 `$null`；受影响时以确切的消息拒绝；`1` 返回确切的文本；`true`、`0`、空值和未设置都拒绝。
- 初始化：`-Platform linux`/`macos` 配合 `10.0.12` 返回 `Failure` 2 `runtime`；`New-MuxAgentProviders` 和 `Get-MuxAgentPlatformMembers` 的模拟不记录任何调用。设置覆盖开关并让模拟的 `New-MuxAgentProviders` 以 2 `state` 拒绝时，`Failure` 保留 `advisory`，入口和启动器会打印它。仅 Unix：受影响版本加覆盖开关时返回 `P.Advisory`。AST：参数默认值恰好是这两个调用；入口和启动器调用 `Initialize-MuxAgentProviders` 时只传 `-Entry`；没有其他地方调用 `New-MuxAgentProviders`。
- 回复、`-Text`、watch 画面、启动器的 stderr 行和窗格环境（有和没有覆盖开关）：每项都分别测试有和没有 `Advisory`。
- 锁：对三种异常类型中的每一种，抛出一次的工厂产生两次创建、一次 `100` 的休眠和一次操作运行；抛出两次的工厂以 2 `lock` 拒绝并指出 `XDG_RUNTIME_DIR`。来自名称的拒绝：不创建、不休眠、不运行操作。不重试，只创建一次且不休眠：其他异常类型、`WaitOne` 超时、被放弃的互斥体、`WaitOne` 抛出的 `IOException`（不运行操作；互斥体被释放）、操作内部的拒绝。重试获取之后，另一个 runspace 以超时 0 获取同一名称，证明已释放。
- 文档：README 和 Home 以两种语言写入新的锁段落；Release-v0.1.0 发布说明保留原段落。
- 运行时证明：CI 的集成作业在运行器的 pwsh 上运行真实的入口和启动器；那里的一个测试要求 `fixed`，并打印版本。

## 必须通过

在 Linux、macOS 和 Windows 上运行 `pwsh -NoProfile -File tests/Invoke-MuxAgentTests.ps1`，并在 Linux 和 macOS 上运行 `-Path tests/integration -Integration`。

## 非目标

替换命名互斥体；v0.1.1；Windows；其他覆盖开关。

## 先例

| 做法 | 适用性 |
|---|---|
| Go `GOTOOLCHAIN` | 拒绝过旧的工具链 |
| pip `--ignore-requires-python` | 覆盖开关以它忽略的内容命名 |
| .NET `UnsafeDefaultMutexScopeGlobal` | 选择退出安全默认值时名称直说 |
| .NET SDK NETSDK1238 | 对易受攻击运行时的警告，作为 `advisory` 保留 |
| npm `engine-strict` | 默认不拒绝：已否决 |

## Rust 实现（0.2.0）

0.2.0 不运行 .NET，所以本页应对的公告不再适用。

- **入口。** `scripts/mux-agent`（POSIX sh）或 `scripts\mux-agent.cmd` 依次尝试 `MUX_AGENTS_BIN`、`scripts/bin/<os>-<arch>/mux-agent[.exe]` 和 `target/release/mux-agent[.exe]`，运行第一个可用的；都不可用时打印 JSON `entry` 拒绝并以 1 退出。
- **锁。** 在 Unix 上，锁是 passwd 主目录下 `.local/state/mux-agents/locks` 中的文件，用 `File::try_lock` 获取；Windows 使用命名互斥体 `Local\MuxAgents-<hex>`。锁按用户划分，同一用户的所有状态根共用它们，`GHSA-2j93-m82w-73j2` 不再适用。
- **进程层。** 不启动辅助进程：Linux 读取 `/proc` 并通过 `pidfd` 结束进程；macOS 使用 `proc_pidinfo`、`KERN_PROCARGS2` 和 `sandbox_check`；Windows 使用 Toolhelp、`GetProcessTimes`、`ProcessCommandLineInformation`、进程令牌和 DACL。
- **交付。** `main` 只有源码。标签 `v<version>` 把六个二进制文件构建到 `scripts/bin/`，附带一个 `SHA256SUMS`，并提交到 `.claude-plugin/marketplace.json` 固定的 `dist` 分支；`dist` 只会移到更新的版本。
- **运行时检查。** `runtime` 检查、`advisory` 和 `MUX_AGENTS_ALLOW_AFFECTED_DOTNET` 已移除：没有代码读取这个变量，也没有回复带有 `advisory`。
