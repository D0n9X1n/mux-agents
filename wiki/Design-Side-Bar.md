# Side bar: how many agents, and what each is doing

[简体中文](Design-Side-Bar-zh-CN)

Sub-project C of milestone v0.1.0, issue #4. Builds on A and B.

## Purpose

In the main chat session, show a side bar with the agent count and one line per agent saying what it is doing, for both clients.

## Decisions (PM's calls under the release goal)

1. **One feed, two views.** `list -All` is the feed. Claude Code draws it in a pane beside the chat. Every client, Copilot CLI included, can open it as a narrow multiplexer split.
2. **"Doing" never scrapes the screen** (a client's last line is its footer). In order: the agent's last `status`, the first line of its last sent message, the first line of its task.
3. **One plugin.** Copilot CLI 1.0.105 loads a plugin carrying a hooks module without warning (local probe), so the Claude Code pane ships inside `mux-agents`.

## Verbs

- **`status (-Body <text> | -BodyFile <path>)`.** Runs from an agent pane only. A human pane refuses `target` ("only an agent reports status"). Agents must use `-BodyFile`, as with `send`. Keeps the first non-empty line, with control characters removed and cut to 80 characters. Writes `status.json` (`text`, `at`) in the agent's folder and returns `{ name, text }`.
- **`list` gains fields per agent:**
  - `state`: `running`, or for an ended agent `exited`, `stopped` or `lost` from `notice.json`.
  - `doing` and `doingFrom` (`status`, `message` or `task`).
  - `age`: seconds since `createdAt`.
  - `unread`: messages from this agent that the caller has not read. Counting them does not move the caller's read offset.
  - `-All` adds the session's ended agents, newest first, at most 10.
  - The result gains `running`, the count of live agents.
- **`sidebar [-Close]`.** Refused (`tagged`) only from a tagged child pane (`AgentTag` set); the main Claude Code or Copilot chat (an untagged agent caller) and a plain shell may run it. Opens `split-window -d -h -l 42` beside the caller running `pwsh -NoProfile -File <entry> watch` (retry without `-l` if rmux refuses it), tagged `@agent-role sidebar`. One per session: an existing one is returned with `opened: false`; `-Close` kills it. No `@agent-id`, so it is outside the cap and `list`.
- **`watch`.** The side-bar loop: every 2 s it asks its own pane, reads the feed in-process and redraws (ANSI home and clear) a count line `mux-agents · 3 running`, then per agent `● claude-1  claude  4m` and an indented `doing` line; ended agents show `○ copilot-1  exited 0`. A pure `Format-MuxAgentBoard` builds the text. A frame drawn after a failed board read or an unanswered probe of its pane ends with `refresh failed`. It ends `pane-closed` when its pane is gone, `output-closed` when a write fails, and `probe-failed` after 30 unanswered probes in a row (a minute). It draws only to a terminal: with stdout redirected it refuses `usage`, so it runs only as the side-bar pane's own command.

`spawn` writes `taskLine` (the task's first non-empty line, 80 characters) into `record.json`. `send` from an agent writes `last-sent.json` (`line`, `at`) in the sender's folder. Every `doing` source (status, last line, task line) passes `ConvertTo-MuxAgentMessageText` and loses tabs before storage, and `Format-MuxAgentBoard` strips C0, DEL and C1 controls again before drawing.

## Claude Code pane

`hooks/hooks.json` names `hooks/register.tsx`, and `plugin.json` names `types/index.d.ts`, which declares the pane's state:

- **Opens** after a `Bash` or `PowerShell` `tool.call` that runs `mux-agent.ps1 spawn`, is not an error and whose output holds a successful spawn reply (JSON or `-Text`), when `TMUX` or `RMUX` is set: `$.ui.open({ id: 'mux-agents', title: 'Agents' })`. `/mux-agents` opens it on demand. Children never spawn, so only the starting session opens it.
- **Refresh:** `$.clock.every(3000)` runs `$.process.run([pwsh, -NoProfile, -File, <plugin root>/scripts/mux-agent.ps1, list, -All])` into an atom and sets `$.ui.status('agents: <running>')`. A refresh starts only after the previous one has settled; each run times out after 10 s.
- **Draws** the count line, then per agent name, client, a dim `doing` and age; ended agents dimmed; a failed refresh keeps the last board, marked `refresh failed`. Every drawn string loses C0, DEL and C1 controls, and tabs become spaces.

## Skill

`SKILL.md` gains: agents run `status -BodyFile <file>` when they start a step; in Copilot CLI, run `sidebar` once after your first `spawn`; `list -All` shows ended agents.

## Component map

| component | job | interface | reuses or extends | likely change | test seam |
|---|---|---|---|---|---|
| `Invoke-MuxAgentStatus` | store an agent's status | `status` verb | `Read-MuxAgentText`, `ConvertTo-MuxAgentMessageText`, lock | more fields | fake providers |
| `Get-MuxAgentBoard` | the feed | dictionary like `list` | `Invoke-MuxAgentList`, notices, mailboxes | columns | fake providers |
| `Format-MuxAgentBoard` | text for `watch` | board → string | none | layout | pure unit tests |
| `Invoke-MuxAgentSidebar` / `Watch` | open, find, close the split; loop | verbs | `Invoke-MuxAgentNative`, pane tags | size | fake rmux rules; tmux integration |
| `hooks/register.tsx` | Claude Code pane | `/mux-agents`; auto-open on spawn | plugin mod API | drawing | `claude plugin test` |

## Tests

- **Unit (all OSes, fake providers).**
  - `status`: truncation, control characters, refusal from a human pane, `-Body` refused for agents.
  - `doing` order: status, then message, then task.
  - `unread` counts without moving the offset.
  - `-All` with each ended kind, capped at 10.
  - `sidebar`: tagging, a second call returns the same pane, `-Close`, the `-l` retry; opened, reused and closed by an untagged Claude caller, an untagged Copilot caller and a shell; refused from a tagged child.
  - `Format-MuxAgentBoard` on fixed boards, including a board whose task line, status and agent name hold ESC sequences, BEL and C1 bytes: the output holds none of them.
- **Integration (ubuntu, macOS).** The B harness runs `sidebar`, sees the `@agent-role` pane, checks `capture-pane` of it shows the count and the fake agent's `status` line, then `-Close`.
- **Claude Code pane.** `tests/claude/register.test.tsx` and `tests/claude/board.test.ts` under `claude plugin test`: a spawn tool call opens the pane, a stubbed `list` renders count and rows, a failed refresh keeps the last board. The lint job installs Claude Code 2.1.310 and runs `claude plugin validate .` and `claude plugin test .`.

## Must-pass

`pwsh -NoProfile -File tests/Invoke-MuxAgentTests.ps1` and the integration jobs are green on all runners, and `claude plugin test .` passes.

## Non-goals

Transcript inference; `pane-border-format`; a Copilot CLI panel; notifications.

## Prior art

| practice | source | fit with our constraints | evidence |
|---|---|---|---|
| Pane user options carrying an agent's state | ntm `@ntm_agent_mail_state` | Same role as `@agent-role` and `status` | survey |
| Status from files the agents write | Claude Code agent teams inboxes | Same: `status.json`, mailboxes | survey |
