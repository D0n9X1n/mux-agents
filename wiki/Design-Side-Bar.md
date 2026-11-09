# Side bar: how many agents, and what each is doing

[简体中文](Design-Side-Bar-zh-CN)

Sub-project C of milestone v0.1.0, issue #4. Builds on A and B. This page describes the bar as it is now; History says how it got here.

## Purpose

In the main chat session, show a side bar with the agent count and one line per agent saying what it is doing, for both clients.

## Decisions (PM's calls under the release goal)

1. **One feed, two views.** `list -All` is the feed. Claude Code draws it in a pane beside the chat. Every client, Copilot CLI included, gets it as a narrow multiplexer split.
2. **"Doing" never scrapes the screen** (a client's last line is its footer). In order: the agent's last `status`, the first line of its last sent message, the first line of its task.
3. **One plugin.** Copilot CLI 1.0.105 loads a plugin carrying a hooks module without warning (local probe), so the Claude Code pane ships inside `mux-agents`.
4. **The bar is a process, not a label.** The session records the pane and the `watch` process `sidebar` started; `@agent-role sidebar` is only a label, and rmux's `rotate-window` leaves labels behind (#47). Only the recorded process is ever moved, resized or closed.
5. **The bar keeps its place.** It belongs at the right edge, full height, so the user never has to put it back; it is put back without moving the user's focus.

## Verbs

- **`status (-Body <text> | -BodyFile <path>)`.** Runs from an agent pane only. A human pane refuses `target` ("only an agent reports status"). Agents must use `-BodyFile`, as with `send`. Keeps the first non-empty line, with control characters removed and cut to 80 characters. Writes `status.json` (`text`, `at`) in the agent's folder and returns `{ name, text }`.
- **`list` gains fields per agent:**
  - `state`: `running`, or for an ended agent `exited`, `stopped` or `lost` from `notice.json`.
  - `doing` and `doingFrom` (`status`, `message` or `task`).
  - `age`: seconds since `createdAt`.
  - `unread`: messages from this agent that the caller has not read. Counting them does not move the caller's read offset.
  - `-All` adds the session's ended agents, newest first, at most 10, and `endedTotal`, the count of all of them.
  - The result gains `running`, the count of live agents.
- **`sidebar [-Close]`.** Refused (`tagged`) only from an agent pane; the main Claude Code or Copilot chat and a plain shell may run it. One bar per session, under the session's sidebar lock: the proven bar is returned with `opened: false`, else a new one opens. `-Close` first sets the session option `@mux-agents-sidebar` to `off`, so no later spawn reopens it, then kills the bar only while it is still the recorded process; `sidebar` unsets the option. A pane that carries `@agent-role sidebar` but cannot be proven the bar refuses `1 sidebar` and is named, never closed. The bar has no `@agent-id`, so it is outside the cap and `list`.
- **`watch`.** The bar's loop: every 2 s, and at once after a key or a left press in the bar, it reads its own pane's size, reads the feed in-process and draws the board at that size. A frame drawn after a failed board read or an unanswered probe of its pane ends with `refresh failed`. It ends `pane-closed` when its pane is gone, `output-closed` when a write fails, `probe-failed` after 30 unanswered probes in a row (a minute), and `interrupted` on Ctrl-C typed in the bar; it restores the terminal on every exit. It draws only to a terminal: with stdout redirected it refuses `usage`, so it runs only as the bar pane's own command. Only the bar's own `watch` keeps the layout; one run by hand draws and takes clicks and keys. It reads its terminal only when stdin and stdout are both terminals and `TERM` is not `dumb`; otherwise it only draws. On Windows it reads the console: VT input with line editing, echo, Ctrl-C processing and quick edit off, waits on the input handle and reads key records (never `ReadFile`, which a resize record would block), and a console control handler puts the mode back on Ctrl-C, Ctrl-Break or close.

`spawn` writes `taskLine` (the task's first non-empty line, 80 characters) into `record.json`. `send` from an agent writes `last-sent.json` (`line`, `at`) in the sender's folder. Every `doing` source (status, last line, task line) loses control characters and tabs before storage, and the bar strips C0, DEL and C1 controls again before drawing, so no field moves the cursor or adds a line.

## The bar

- **Where.** `sidebar`, or the first successful `spawn` of a session, runs `split-window -d -f -h` at the right edge of the caller's window, full height, running `<exe> watch` by the binary's absolute path; `-d` keeps the caller's pane active. Later spawns reuse it; after `sidebar -Close` it opens again only on `sidebar`. A spawn that cannot open it still succeeds and reports `sidebarError`. In tab mode the bar stays in the window you spawned from.
- **Width.** `max(@mux-agents-sidebar-min, window × @mux-agents-sidebar-percent / 100)`, by default `max(40, window × 20 / 100)`. Both options are read where the bar's pane stands (pane, window, session, then `set -g`), as format fields in queries that already run, so they cost no extra call; a value that is not a whole number in range uses its default. The split asks for the width with `-l` and sets it again with `resize-pane`, since rmux ignores `-l`. Why: a fixed width wastes a wide window, and a share alone is unreadable in a narrow one.
- **Keeper.** Each tick the bar checks its own window. When a split, a layout change, `swap-pane` or `rotate-window` moved it, it is put back with `move-pane -d -f -h`, its width is set again, and the pane that was active before is selected again, so the user's focus never moves. A width the user dragged is kept until the window's width or a width option changes. A zoomed window is left alone, so the bar never unzooms a pane. After 3 failed repairs in a row it stops trying and shows `layout: repair paused` until the layout changes or a minute passes, so it never fights a layout it cannot fix.
- **Redraw.** Every 2 s, and at once after a key or a left press, the board is laid out at the pane's actual size and never wraps: long text is cut with `...`, and a short pane drops blank lines first, then ended rows, then the "doing" lines and whole rows of the oldest agents; the header stays. Each frame is written in place inside synchronized output (`ESC[?2026h` … `ESC[?2026l`): cursor home, each row, erase to the end of the line, erase below. There is no full clear, so the bar does not flicker. With `TERM=dumb` it prints plain frames instead.
- **Look.** A live agent shows its activity as herdr names it, which its own client reports with `report` (#82): `! needs you` (bold yellow, the word reversed), `✦ done` (bold cyan: finished while you were not looking), `◉ working` (green) or `◌ idle` (dim row); rows are ranked in that order, the most recent change first, and the time is since that change. When you leave an agent's pane the bar writes its `seen.json`, so a done agent turns idle only after you have looked at it. The 3 newest ended agents that did not end `lost` show `✓ done`, `✗ exit N` (red) or `- stopped` (yellow), then `+K more` (a lost agent is counted, never drawn). The hint names `list -All` only when every ended agent is among the 10 it shows, else it reads `+K more ended`, so the bar never promises rows `list -All` does not show. Each live agent's name has its own color, the one least used by the session's live agents and never a state color, so names and states never look alike. Color follows `NO_COLOR` (off), `FORCE_COLOR` or `CLICOLOR_FORCE` (on), then `TERM=dumb` (off); `MUX_AGENTS_ASCII=1` draws ASCII glyphs for terminals without the symbols.
- **Identity (#47).** The split's process must be this binary's `watch`; only then is the pane labelled and recorded in the session option `@mux-agents-sidebar-id` as `<pane> <pid> <start>`. Each later step proves the recorded pane is alive and still that process. A labelled pane that is not it is refused and named, never closed: it may be an old bar, or the user's own pane carrying a label rmux moved.

## Claude Code pane

`hooks/hooks.json` names `hooks/register.tsx`, and `plugin.json` names `types/index.d.ts`, which declares the pane's state. The pane is separate from the multiplexer bar: `sidebar -Close` does not close it.

- **Opens** after a `Bash` or `PowerShell` `tool.call` that runs the entry's `spawn`, is not an error and whose output holds a successful spawn reply (JSON or `-Text`), in a terminal CLI session inside tmux or rmux: `$.ui.open({ id: 'mux-agents', title: 'Agents' })`. `/mux-agents-pane` opens it on demand (the skill owns `/mux-agents`). Children never spawn, so only the starting session opens it.
- **Refresh:** `$.clock.every(3000)` runs the entry's `list -All` into an atom and sets `$.ui.status('agents: <running>')`. A refresh starts only after the previous one has settled; each run times out after 10 s.
- **Draws** the count line, then per agent name, client, `[model]`, a dim `doing` and age; ended agents dimmed; a failed refresh keeps the last board, marked `refresh failed`. Every drawn string loses C0, DEL and C1 controls, and tabs become spaces.

## Skill

`SKILL.md` says: agents run `status -BodyFile <file>` when they start a step; the first `spawn` opens the side bar, `sidebar -Close` closes it for the session and `sidebar` opens it again; `list -All` adds ended agents (at most 10).

## Component map

| component | job | interface | reuses or extends | likely change | test seam |
|---|---|---|---|---|---|
| `agents::view` | store an agent's status; `list` | `status`, `list` verbs | `mail::doing_line`, the agent lock | more fields | fake providers |
| `agents::board` | the feed | `{running, endedTotal, agents}` | records, notices, mailboxes | columns | fake providers |
| `sidebar` `layout` / `frame` | the bar's rows and frame | board → rows → text | `paint` | layout | pure unit tests |
| `sidebar` open, keeper, `watch` | open, find, close the bar; keep its place; loop | verbs | `mux::native`, session options | size | fake multiplexer scripts; live tmux and rmux |
| `paint` | color switches, colors, display columns, cutting | functions | `unicode-width` | glyphs | unit tests |
| `hooks/register.tsx` | Claude Code pane | `/mux-agents-pane`; auto-open on spawn | plugin mod API | drawing | `claude plugin test` |

## Tests

- **Unit (all OSes, fake providers).**
  - `status`: truncation, control characters, refusal from a human pane, `-Body` refused for agents.
  - `doing` order: status, then message, then task.
  - `unread` counts without moving the offset.
  - `-All` with each ended kind, capped at 10, with `endedTotal` counting all.
  - `sidebar`: one bar per session, the `-l` retry, `-Close`, refused from an agent pane; a bar is proven only by its live recorded `watch`; an unproven label refuses and nothing is closed.
  - Layout on fixed boards at many sizes, emoji and ZWJ sequences included, and a board whose fields hold ESC sequences, BEL and C1 bytes: the output holds none of them.
  - Keeper: a moved bar goes back and the focus stays, a zoomed window is left alone, 3 failures pause repair, a changed window or option resizes.
- **Live (Linux, macOS).** Real tmux 3.8, tmux 3.2a and rmux 0.10.0 servers: the bar opens at the right edge at its width, draws the board, goes back after the layout is damaged, and `-Close` closes it.
- **Windows console (#70).** The test binary runs itself again with a hidden console of its own: setup takes the raw mode, typed records come back as their bytes, a resize alone never ends a wait early, Drop restores the mode, and Ctrl-Break after or during setup leaves the mode restored.
- **Live (Windows).** `tests/live_windows.rs` in a private rmux 0.10.0 server (a debug build in CI, since the release daemon cannot start in a runner's job object, rmux#238): `j` then Enter and a click on the bar focus an agent, and Ctrl-C ends a hand-run `watch` with mouse reports off.
- **Claude Code pane.** `tests/claude/register.test.tsx` and `tests/claude/board.test.ts` under `claude plugin test`: a spawn tool call opens the pane, a stubbed `list` renders count and rows, a failed refresh keeps the last board. The lint job installs Claude Code 2.1.310 and runs `claude plugin validate .` and `claude plugin test .`.

## Must-pass

`cargo test --features test-panic --locked` on every OS, `cargo test --features live,test-panic --locked --test live` on Linux and macOS, `cargo test --features live,test-panic --locked --test live_windows` on Windows, and `claude plugin test .`.

## Non-goals

Transcript inference; `pane-border-format`; a Copilot CLI panel; notifications.

## Prior art

| practice | source | fit with our constraints | evidence |
|---|---|---|---|
| Pane user options carrying an agent's state | ntm `@ntm_agent_mail_state` | Same role as `@agent-role` and `status` | survey |
| Status from files the agents write | Claude Code agent teams inboxes | Same: `status.json`, mailboxes | survey |

## History

- **0.1.0.** `sidebar` opened `split-window -d -h -l 42` beside the caller, running the PowerShell entry's `watch`, and found it by its `@agent-role sidebar` label. `watch` cleared the screen every 2 s and drew `mux-agents · 3 running`, then `● claude-1  claude  4m` rows. In Copilot CLI you ran `sidebar` yourself after the first `spawn`.
- **0.2.1.** The first successful `spawn` opens the bar; `sidebar -Close` turns it off for the session (#35).
- **0.2.2.** The bar is found by its process, not its label (#47, #50), kept at the right edge, full height, `max(40, window / 5)` columns wide; colors by state and agent, `NO_COLOR`, ASCII glyphs and an in-place redraw with synchronized output (#51).
- **0.2.4.** The width is `max(@mux-agents-sidebar-min, window × @mux-agents-sidebar-percent / 100)`, by default 30 columns and 5% (#59).
- **0.2.5.** A left click on an agent's name line, or `j`/`k` and Enter in the bar, switches to that agent's window and pane; `◉`/`◌` in agent text draw as `•` (#61).
- **0.2.6.** The default width is back to 20% of the window, at least 40 columns (#74). Clicks and keys work on Windows too: `watch` reads the console in VT input mode and restores it on every exit (#70).
- **Next.** herdr's activity: needs you, done until seen, working and idle, ranked, with the time since the change and the header counting each state (`N need you` first) instead of `N running`; `◌ waiting` becomes `◌ idle` (#82). herdr's section headings when the live agents are in more than one state, and one tmux message when an agent starts needing you or finishes while you look elsewhere, `@mux-agents-alerts` to quiet it (#83).

## Rust (0.2.0)

0.2.0 keeps the feed, the pane and the split; the side bar and the agent panes run the binary itself.

- **Entry.** `scripts/mux-agent` (POSIX sh) or `scripts\mux-agent.cmd` runs the first usable of `MUX_AGENTS_BIN`, `scripts/bin/<os>-<arch>/mux-agent[.exe]` and `target/release/mux-agent[.exe]`; with none it prints a JSON `entry` refusal and exits 1.
- **Locks.** On Unix a lock is a file under the passwd home's `.local/state/mux-agents/locks`, taken with `File::try_lock`; Windows uses a named mutex `Local\MuxAgents-<hex>`. Locks are per user, so every state root of one user shares them, and `GHSA-2j93-m82w-73j2` no longer applies.
- **Process layer.** No helper processes: Linux reads `/proc` and kills through `pidfd`; macOS uses `proc_pidinfo`, `KERN_PROCARGS2` and `sandbox_check`; Windows uses Toolhelp, `GetProcessTimes`, `ProcessCommandLineInformation`, the process token and DACLs.
- **Delivery.** `main` is source only. A tag `v<version>` builds six binaries into `scripts/bin/` with one `SHA256SUMS` and commits them on the `dist` branch, which `.claude-plugin/marketplace.json` pins; `dist` only moves to a newer version.
- **Panes.** An agent pane runs `<exe> launcher <spec.json>` and the side bar runs `<exe> watch`, each by the binary's absolute path, so a pane keeps the binary it started with. The Claude Code pane runs `sh <root>/scripts/mux-agent list -All`, or `cmd.exe /d /c <root>\scripts\mux-agent.cmd list -All` when `OS` is `Windows_NT`, and still opens after a 0.1 command line that ran `mux-agent.ps1 … spawn`.
