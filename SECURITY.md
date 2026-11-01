# Security policy

## Supported versions

Only the latest release gets security fixes. Upgrade with the one-line command in the [README](https://github.com/D0n9X1n/mux-agents#quick-start).

| Version | Supported |
|---|---|
| 0.2.x | yes |
| 0.1.x | no: it uses .NET named mutexes affected by [GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2) on Linux and macOS |

## Reporting a vulnerability

Please do not open a public issue. Report it privately through GitHub: [open a draft security advisory](https://github.com/D0n9X1n/mux-agents/security/advisories/new).

Include what you ran, what happened, and the JSON reply or log you saw, with the OS, multiplexer (tmux or rmux and its version) and client (Claude Code or Copilot CLI). You will get an answer within seven days. Once a fix is released, the advisory is published with credit unless you ask otherwise.

## Scope

mux-agents starts AI coding agents with permission prompts off, so its job is never to give an agent more than its caller already has. Reports that matter most:

- a way to spawn from a sandboxed, non-bypass or tagged (agent) caller, or past the six-agent cap;
- a way to make `mux-agent` read, write, signal or kill outside its private state folder, its own panes or its own processes;
- PID-reuse, symlink or lock races that cross users or redirect state;
- anything that leaks a secret into a reply, a log or another pane.

The safety checks are described on the wiki's [Usage](https://github.com/D0n9X1n/mux-agents/wiki/Usage#safety-checks) page.
