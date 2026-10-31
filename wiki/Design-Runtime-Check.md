# Runtime check: refuse a .NET affected by GHSA-2j93-m82w-73j2

[简体中文](Design-Runtime-Check-zh-CN)

## Purpose

On Linux and macOS, .NET 8.0.0–8.0.31, 9.0.0–9.0.20, 10.0.0–10.0.12 and 11.0 previews through RC 1 let another local user erase or replace file contents, loosen folder permissions or gain higher privileges through named mutexes and shared memory ([GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2)). Every verb, `watch` and the launcher open a named mutex there (`Use-MuxAgentLock`). Issue #25: refuse such a runtime unless the user opts in by name.

## Decisions (PM's calls under the goal "address all issues")

1. Refuse and keep the named mutex. No source says a process without a named mutex is safe on an affected runtime, and a fixed PowerShell exists for every line (7.4.21, 7.5.12, 7.6.7).
2. Override: `MUX_AGENTS_ALLOW_AFFECTED_DOTNET` set to exactly `1`. Replies then carry `advisory`.
3. The version is the running runtime's `AssemblyInformationalVersion` (of the assembly holding `[object]`), never `$PSVersionTable`.
4. Windows is not checked.
5. Fixed runtimes can fail to create a named mutex (a uid with no user-database entry; dotnet/runtime#136236 on 10.0.13). Creation is retried once; a second failure refuses 2 `lock` naming `XDG_RUNTIME_DIR`.
6. Land on main, no v0.1.1 tag; the v0.1.0 zip predates the check.

## Version rule

`Get-MuxAgentRuntimeVerdict -Version <s>` returns `fixed` or `affected`. The whole string must match `^(\d{1,9})\.(\d{1,9})\.(\d{1,9})(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]*)?\z` (`\z`, not `$`, which accepts a final LF); anything else is affected. The label is GA when absent or `servicing.…`; `rc.N.…` is release candidate N (N numeric); `preview.…`, `alpha.…`, `beta.…` precede RC 1; any other label is affected. Fixed: minor 0 and GA with major 8 and patch ≥ 32, 9 and ≥ 21, 10 and ≥ 13; major 11, minor 0, GA or RC N ≥ 2; any major ≥ 12. Everything else is affected, including majors below 8 and a nonzero minor in 8–11.

## Behavior

- `Get-MuxAgentRuntimeAdvisory -Platform -Version -Environment` returns `$null` on Windows or a fixed runtime, override or not. On an affected Linux or macOS runtime it refuses 2 `runtime`: `mux-agents refuses .NET <version> on <platform>: GHSA-2j93-m82w-73j2 (CVE-2026-96231). Install PowerShell 7.4.21, 7.5.12, 7.6.7 or later, or set MUX_AGENTS_ALLOW_AFFECTED_DOTNET=1 to run anyway.` With the override it returns `.NET <version> is affected by GHSA-2j93-m82w-73j2; running because MUX_AGENTS_ALLOW_AFFECTED_DOTNET=1.`
- `Initialize-MuxAgentProviders -Entry [-Platform] [-RuntimeVersion]` (defaults: `Get-MuxAgentPlatform`, `Get-MuxAgentRuntimeVersion`) runs the advisory first, then `New-MuxAgentProviders`, and sets `$P.Advisory`. A refusal returns as `Failure` before any provider, mutex or state folder; a later build failure carries `advisory`.
- `Invoke-MuxAgentCommand` adds `advisory` to success and failure replies when `$P.Advisory` is set; `-Text` prints `advisory: <text>`.
- The launcher writes `mux-agent: <advisory>` to stderr before starting the client; on `Failure` it prints the reply and exits without starting the client (unchanged).
- Each `watch` frame ends with `advisory: GHSA-2j93-m82w-73j2` when `$P.Advisory` is set.
- `Get-MuxAgentPaneEnvironment` adds `-e MUX_AGENTS_ALLOW_AFFECTED_DOTNET=1` on Unix when the caller has the override, so new agent and side-bar panes, which get the server's environment, run too.
- `Use-MuxAgentLock` resolves the name once (a refusal there is not retried), then creates the mutex through `$script:MuxAgentNewMutex`. Only `IOException`, `UnauthorizedAccessException` and `WaitHandleCannotBeOpenedException` from creation are retried, once, after `& $script:MuxAgentLockSleep 100`. `WaitOne`, abandonment and the action are outside the retry. Callers that absorb lock failures (watch's `refresh failed`, the launcher's notice) are unchanged.

## Component map

| Piece | File | Extends |
|---|---|---|
| Version, verdict, advisory | `scripts/MuxAgents.ps1` | beside `Get-MuxAgentPlatform` |
| Check, `Advisory` | `scripts/MuxAgents.ps1` | `Initialize-MuxAgentProviders` |
| Replies, frames, panes | `scripts/MuxAgents.ps1` | `Invoke-MuxAgentCommand`, `Start-MuxAgentWatch`, `Get-MuxAgentPaneEnvironment` |
| Retry seams | `scripts/MuxAgents.ps1` | `Use-MuxAgentLock` |
| Launcher line | `scripts/mux-agent-launcher.ps1` | after `Initialize-MuxAgentProviders` |
| Docs | `README.md`, `wiki/Home*.md`, this page and its twin | the lock paragraph |

## Tests

- Verdict, every OS: `8.0.31`/`8.0.32`, `9.0.20`/`9.0.21`, `10.0.12`/`10.0.13`, `10.0.14-servicing.26522.126+0427b9a`, `11.0.0-preview.7.1`, `11.0.0-rc.1.26425.128`, `11.0.0-rc.2.26504.105`, `11.0.0-rc.10.1`, `11.0.0`, `11.0.1-servicing.1`, `12.0.0`, `7.0.20`, `10.1.0`, `10.0.13-weird.1`, `10.0.13x`, `99999999999.0.0`, `10.0.14` followed by LF, `''`.
- Reader: `Get-MuxAgentRuntimeVersion` equals the live `AssemblyInformationalVersionAttribute` of `[object].Assembly`, and its numeric part equals `[Environment]::Version`.
- Advisory: Windows `$null` when affected; fixed Linux/macOS `$null` with and without the override; affected refuses with the exact message; `1` returns the exact text; `true`, `0`, empty and unset refuse.
- Initialize: `-Platform linux`/`macos` with `10.0.12` returns `Failure` 2 `runtime`; mocks of `New-MuxAgentProviders` and `Get-MuxAgentPlatformMembers` record no call. With the override and a mocked `New-MuxAgentProviders` that refuses 2 `state`, `Failure` keeps `advisory`, and the entry and launcher print it. Unix-tagged: an affected version with the override returns `P.Advisory`. AST: the parameter defaults are exactly the two calls; the entry and the launcher call `Initialize-MuxAgentProviders` with only `-Entry`; nothing else calls `New-MuxAgentProviders`.
- Replies, `-Text`, the watch frame, the launcher's stderr line and the pane environment (override present and absent): each with and without `Advisory`.
- Lock: for each of the three exception types, a factory that throws once makes two creations, one sleep of `100` and one action run; one that throws twice refuses 2 `lock` naming `XDG_RUNTIME_DIR`. A refusal from the name: no creation, sleep or action. Not retried, one creation and no sleep: another exception type, a `WaitOne` timeout, an abandoned mutex, an `IOException` thrown by `WaitOne` (no action; the mutex is disposed), a refusal inside the action. After a retried take, another runspace takes the same name with timeout 0, proving the release.
- Docs: README and Home carry the new lock paragraph in both languages; the Release-v0.1.0 notes keep theirs.
- Runtime proof: CI's integration jobs run the real entry and launcher on the runner's pwsh; a test there requires `fixed` and prints the version.

## Must-pass

`pwsh -NoProfile -File tests/Invoke-MuxAgentTests.ps1` on Linux, macOS and Windows, and `-Path tests/integration -Integration` on Linux and macOS.

## Non-goals

Replacing the named mutex; v0.1.1; Windows; other overrides.

## Prior art

| Practice | Fit |
|---|---|
| Go `GOTOOLCHAIN` | refuse a toolchain that is too old |
| pip `--ignore-requires-python` | an override named for what it ignores |
| .NET `UnsafeDefaultMutexScopeGlobal` | opting out of the secure default says so |
| .NET SDK NETSDK1238 | a warning on a vulnerable runtime, kept as `advisory` |
| npm `engine-strict` | refusal off by default: rejected |
