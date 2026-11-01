# Cross-platform: tmux on Linux and macOS, rmux on Windows

[简体中文](Design-Cross-Platform-zh-CN)

Sub-project B of milestone v0.1.0, issue #1. Builds on A.

## Purpose

Run every verb on Linux and macOS under tmux 3.2+, with Windows' checks and refusal names. Windows behavior stays as it is.

## Decisions

1. **One shared core.** OS-specific code sits behind `$P` (`New-MuxAgentProviders`): Windows members are today's code, unchanged; Unix members fill the same names; only the factory branches.
2. **Fail closed:** unreadable signals refuse.
3. **Process identity is the kernel start value, compared exactly.** Linux: raw `starttime` ticks after the last `)` of `/proc/<pid>/stat`. macOS: `Process.StartTime` (microseconds).
4. **Clients are identified by real executable path**, not basename alone (the Claude installer links `claude` into `versions/<v>`).
5. **Locks and state.** Named mutexes stay; on Unix the name is `Global\MuxAgents-<uid>-<hash>`, since `Local\` is one session and every tmux pane is its own session. State: `$MUX_AGENTS_STATE` (absolute, else refused), else an absolute `$XDG_STATE_HOME` + `/mux-agents/state`, else `~/.local/state/mux-agents/state`. Each new pane gets the selected root with `tmux -e`, because a pane starts with the server's environment.

## Unix provider members

| member | Linux | macOS |
|---|---|---|
| `Processes` (pid, ppid, start, path, argv) | `/proc/[0-9]*`: `stat`, `exe` link, NUL-split `cmdline` | `ps -A -o pid=,ppid=`; start and path from `Process`; argv from `sysctl KERN_PROCARGS2` (Add-Type) |
| `Open` / `Kill` | `pidfd_open`, then the start; Kill sends SIGKILL on that fd, which cannot reach a reused PID, and waits for POLLIN without POLLERR or POLLNVAL; no pidfd → no signal, and only ESRCH from `kill(pid, 0)` counts as exited | no identity-bound kill exists: never kills; only ESRCH counts as exited, else reported |
| `Sandbox` (C1) | refuse `sandbox` when `/proc/self/status` is unreadable, `NoNewPrivs: 1`, or `SANDBOX_RUNTIME=1` | refuse when `SANDBOX_RUNTIME=1`, or `sandbox_check(getpid(), NULL, 0)` is non-zero or fails |
| `Cwd` (C5) | `/proc/<pid>/cwd` | `proc_pidinfo(PROC_PIDVNODEPATHINFO)` |
| `StateRoot` | the path above, canonical via `realpath` | same |
| `Mux` | `tmux` from the `$TMUX` server PID's executable, else `PATH`; `tmux -V` below 3.2 refuses `endpoint` | same |

Refusal messages name the signal ("NoNewPrivs is set").

## Changed behavior

- **Endpoint.** Unix `$TMUX` must match `^(/[^,\r\n]+),(\d+),(-?\d+)$`. Calls stay `<mux> -S <socket> -N …`; probe rows are validated as today.
- **Caller chain.** Unix shells allowed between caller and agent: `sh`, `bash`, `zsh`, `fish`, `dash`, `ksh`, `tcsh`, `pwsh`. The agent is the nearest process whose real path is `*/claude/versions/*` or basename `claude`, basename `copilot`, or `node` whose script's `realpath` ends in `@github/copilot/npm-loader.js` (node options are read fail-closed: an unknown option names no client). Start values compare exactly; the 100 ns/µs rounding stays Windows-only.
- **C3** reads argv from `Processes` on Unix instead of splitting a string. Allowlists unchanged.
- **C5.** Claude on Unix: the agent's kernel cwd, read between two equal start reads; if `~/.claude/sessions/<pid>.json` exists, its `cwd` must agree. `sessionId`/`procStart` checks stay Windows-only, which removes the false refusal after `/clear` (claude-code#36213). Copilot: `workspace.yaml` `cwd` everywhere.
- **Stop.** `kill-pane` closes the pane and its process group as today. A surviving process is killed only through Linux `pidfd` as above; otherwise it is left and reported (`survivors` with pids), never signalled by PID.
- **State privacy.** Windows ACL checks unchanged. On Unix the walk goes from `/` down. Every existing folder above the root must be owned by root or the user and not group- or other-writable unless sticky (OpenSSH StrictModes); on macOS only deny ACEs pass there, because an allow ACE outranks the mode and the sticky bit. A missing folder is made by `mkdir(2)` at `0700`, which fails on any existing entry, a link included, then `chmod 0700`. The root and every folder below it must be: exactly `0700`; owned by the effective uid; without extended ACL (no `+` in `ls -ld`, and on macOS no ACE lines in `ls -lde`, which also catches inherited entries); not a link on the canonical path; the same dev:inode on two `stat` reads. `id`, `stat`, `ls` and `realpath` start by their full path from a fully qualified `PATH` folder, never from the current directory. Anything else, or a failed check, refuses `state`.
- **Launcher.** Finds `claude`/`copilot` on `PATH` without `.exe`, uses `$PSHOME/pwsh`, removes the parent's client identity variables (removed, not left empty), starts the client unredirected so it owns the pane's terminal.
- **Entry.** Accepts Windows, Linux and macOS; else refuses `platform`.

## Component map

| component | job | interface | reuses or extends | likely change | test seam |
|---|---|---|---|---|---|
| `New-MuxAgentProviders` | picks the member set | `$P` members | existing factory | a new OS | fake providers |
| `MuxAgents.Unix.ps1` | Unix members | factory-bound functions | none | client layouts | fixtures |
| `MuxAgents.Native.Unix.cs` | `pidfd`, `sysctl`, `sandbox_check`, `proc_pidinfo` | static C# methods | Windows `Add-Type` | struct layouts | Linux/macOS CI |
| `tests/integration/` | real tmux | `Invoke-MuxAgentTests.ps1 -Integration` | fake client | more verbs | `integration (<os>)` |

## Tests

- **Unit, all OSes, fake providers:** `stat` with `comm` containing `) (`; `NoNewPrivs`, `SANDBOX_RUNTIME` and unreadable-status refusals; client identity paths; endpoint regex; state-root order; state refusals for mode, owner, `+` ACL and ACE lines; C5 cwd agreement and disagreement; kill: start mismatch after `pidfd_open` sends nothing, a PID reused after the last check is never signalled (the fake's fd stays bound to the old process), macOS and no-pidfd report without killing.
- **Native, ubuntu and macOS:** a real folder given an ACL (`setfacl -m u:nobody:rwx` on Linux where available, `chmod +a "everyone allow read"` on macOS) refuses `state`; on macOS a parent with an allow ACE (`chmod +a "everyone allow add_subdirectory,delete_child"`) refuses before anything is created below it, and a parent's inheritable deny ACE (`chmod +a "everyone deny chown,directory_inherit"`) makes the new `0700` child refuse `state`; a link placed at a folder's path refuses and its target keeps its mode; a file named `id` in the current directory never runs; a real child killed via `pidfd` on Linux.
- **Integration, ubuntu and macOS:** a private `tmux -S` server; a fake `claude` on `PATH` that records argv and waits for a quit file; a pwsh driver in a pane runs `spawn` (window and split), `list`, `send`, the agent's `send parent`, `read -Wait`, `read <name>`, `focus`, `stop`; asserts `@agent-*` tags, mailbox contents, that `read -Wait` waits on an empty mailbox, the `stopped` notice, no survivors, a state root set only in the caller reaching each new pane, and `spawn` from the agent pane refused `tagged`.
- **Windows:** existing suites unchanged.

## Must-pass

`pwsh -NoProfile -File tests/Invoke-MuxAgentTests.ps1` green on all three runners with no `windows` exclusion for new suites; `integration (ubuntu-latest)` and `integration (macos-latest)` green.

## Non-goals

Side bar (C); `CLAUDE_CONFIG_DIR`/`COPILOT_HOME`; BSDs; tmux below 3.2; rmux in CI; real client process trees on Unix.

## Prior art

| practice | source | fit with our constraints | evidence |
|---|---|---|---|
| `-N` never starts a server; gone pane `display-message` exits 0 with empty fields | tmux 3.2 CHANGES; tmux 3.6 probe | Same rules as rmux | survey |
| Kill, then verify survivors | uzi | `stop` kills survivors only by `pidfd` | survey |
| `/proc` start ticks, last-`)` parsing; bwrap `NoNewPrivs`, sandbox-runtime `SANDBOX_RUNTIME=1` | proc_pid_stat(5); WSL test; sandbox-runtime | Identity and C1 signals | survey |

## Rust (0.2.0)

0.2.0 keeps these rules in Rust, and adds rmux on macOS and Linux.

- **Entry.** `scripts/mux-agent` (POSIX sh) or `scripts\mux-agent.cmd` runs the first usable of `MUX_AGENTS_BIN`, `scripts/bin/<os>-<arch>/mux-agent[.exe]` and `target/release/mux-agent[.exe]`; with none it prints a JSON `entry` refusal and exits 1.
- **Locks.** On Unix a lock is a file under the passwd home's `.local/state/mux-agents/locks`, taken with `File::try_lock`; Windows uses a named mutex `Local\MuxAgents-<hex>`. Locks are per user, so every state root of one user shares them, and `GHSA-2j93-m82w-73j2` no longer applies.
- **Process layer.** No helper processes: Linux reads `/proc` and kills through `pidfd`; macOS uses `proc_pidinfo`, `KERN_PROCARGS2` and `sandbox_check`; Windows uses Toolhelp, `GetProcessTimes`, `ProcessCommandLineInformation`, the process token and DACLs.
- **Delivery.** `main` is source only. A tag `v<version>` builds six binaries into `scripts/bin/` with one `SHA256SUMS` and commits them on the `dist` branch, which `.claude-plugin/marketplace.json` pins; `dist` only moves to a newer version.
- **Multiplexer.** The endpoint is `RMUX` when set, else `TMUX`. The binary is the server's image when it is named rmux or tmux, else `rmux` on `PATH` when `RMUX` is set, else `tmux`; `tmux -V` below 3.2 or `rmux -V` below 0.10 refuses `endpoint`. Every call is `<mux> -S <socket> -N …`.
