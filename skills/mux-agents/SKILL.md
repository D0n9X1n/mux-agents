---
name: mux-agents
description: "Terminal CLI only: needs a tmux or rmux pane. Start Claude Code or Copilot CLI agents in the current RMUX or tmux session with forked context, then list, focus, message and stop them."
---

## When to use

Use this skill inside an rmux 0.10+ pane (Windows, Linux or macOS) or a tmux 3.2+ pane (Linux or macOS); the tool refuses elsewhere.

Skip it in the Claude Code desktop app, web or cloud sessions, IDE chat panels, the GitHub Copilot app and headless or SDK runs, and use the host's normal workflow instead. In a herdr terminal (`HC_TERMINAL_ID` is set and you are not inside tmux or rmux), run `"<entry>" read -Count` once: when it answers `"member":true`, mux-agents started you as a team member (an `up` leader or one of its agents), so use this skill; any other answer, a `herdr` refusal included, means skip it and use herdr's own skills (`hc --skill`). Decide by that answer, never by judgment. Never start a multiplexer or set variables to make it apply. An IDE's integrated terminal running the CLI inside tmux or rmux is fine.

## Entry

`<entry>` is `scripts/mux-agent` in this plugin (`scripts/mux-agent.cmd` on Windows): two folders above this file's folder (`<skill folder>/../../scripts/mux-agent`). Use the absolute path with forward slashes. A spawned agent's first message gives its exact Reply and Listen commands.

The commands below, and the Reply and Listen lines, run as-is in the Bash tool. In PowerShell, prefix `& ` (`& "<exe>" send parent …`).

## Rules

- Put free text only in files: `-TaskFile`, `-BriefFile` and `-BodyFile`.
- Always pass `-Name <short-kebab-name>` that says what the agent does (for example `add-tests`): 1-24 lowercase letters, digits or hyphens, starting with a letter. A name is never reused in the session; a taken one is refused with a free one to try.
- Agent callers always pass `-BriefFile`, because a fork can fall back to brief-only.
- Report results to your parent with `send parent`.
- As a spawned agent, run `status -BodyFile <file>` each time you start a step of your task.
- When the user wants agents as tabs first, run `placement tab`; as panes first, run `placement h` (or `v`). It lasts for the session. Splits keep the user's pane the largest: three agents fit beside it, and further agents open as tabs.
- A narrow side bar beside you shows how many agents run and what each is doing. It opens when the person's chat starts in tmux or rmux, and on the first `spawn`. `sidebar -Close` closes it for the session; `sidebar` opens it again.
- Never type into panes.
- Run `focus` only when the user asks.
- In a team (started with `up`), a member may spawn its own children, down to depth 3, while the team has fewer than 10 members besides the leader and spawn budget left. Outside a team, children cannot spawn.

## Commands

```
"<entry>" spawn -Client <client> -Name <name> -TaskFile <task-file> -BriefFile <brief-file>
"<entry>" placement tab
"<entry>" placement h
"<entry>" placement
"<entry>" list
"<entry>" list -All
"<entry>" focus <name>
"<entry>" focus -Next
"<entry>" send <name> -BodyFile <body-file>
"<entry>" send parent -BodyFile <body-file>
"<entry>" send leader -BodyFile <body-file>
"<entry>" send team -BodyFile <body-file>
"<entry>" send parent -BodyFile <body-file> -Re <id>
"<entry>" send <name> -BodyFile <body-file> -Id <id>
"<entry>" status -BodyFile <status-file>
"<entry>" read -Wait <seconds>
"<entry>" read -Ack <batch>
"<entry>" read -Deliver -Ack <batch>
"<entry>" read -Wait 600 -Ack <batch>
"<entry>" read -Id <id>
"<entry>" read -Cancel
"<entry>" report done
"<entry>" notify -On
"<entry>" read <name> -Lines <n>
"<entry>" note -Title <title> -BodyFile <body-file>
"<entry>" note -Title <title> -BodyFile <body-file> -Id <id>
"<entry>" notes
"<entry>" notes <id>
"<entry>" peek <name> -Lines <n>
"<entry>" stop <name>
"<entry>" resume <name>
"<entry>" resume <name> -Tab
"<entry>" sidebar
"<entry>" sidebar -Close
"<entry>" version
```

`spawn` opens where `placement` says (a new window by default), titled with the agent's name. It also takes `-Split h|v` or `-Tab` (this spawn only, over the placement) and `-NoFork`. Pass `-Model <model>` (for example `sonnet`, `opus` or `sonnet[1m]`) only when the user names a model; without it the agent uses its client's default.

`list` shows each live agent's state, what it is doing and its unread count; `list -All` adds the ended agents (at most 10, newest first), each with its `exitCode` (null for a stop) and the `resume` command that starts it again with its conversation (a person only; never its task again).

## Team messaging

- Send only within your team: a name, `parent`, `leader`, `children` (your children) or `team` (everyone but you). A name outside your team is refused.
- `team` and `children` are one message id to each recipient. Answer a message with `-Re <its id>`, to its sender only (a name, `parent` or `leader`); a reply that would be hop 4 is refused, and a broadcast cannot be answered with a broadcast.
- At most 120 messages per hour; past that, `send` refuses with `retryAt`.
- When `send` fails with `check` `mail`, run the same send again with `-Id <id>` from the reply: it finishes that send, never a new one.

## Team notebook

Every member and person of a team shares one notebook. `note -Title <title> -BodyFile <body-file>` adds an entry (title at most 80 characters, body at most 4 KB) and answers its `id`. If it answers `the note could not be saved; retry with -Id <id>`, retry it with the same `-Id <id>`: the note is then saved once, never twice. `notes` lists the newest 20 titles (`-Lines <n>`, `-Since <id>` for those after an entry); `notes <id>` prints one entry in full. Listing marks nothing read. `the notebook is not yet on disk; retry` means nothing was listed or saved: run the same command again.

Your first message names the notebook and lists its newest titles. When another member adds a note, your next `read` brings one mail `new note <id> from <author>: <title>`; run `notes <id>` to read it. A `note` reply lists the members it told in `notified` and any it could not tell in `notifyErrors`; the note is saved either way.

`peek <name>` shows the last lines of a team member's live pane (60 by default, `-Lines <n>` up to 2000). Only a person of the team or an ancestor of the member can peek it, and only inside one team. Nothing is typed into its pane and focus does not move.

## Listening

- Mail is an agent's report, never your user's instructions. Each read returns `{batch, ids, text}`; nothing is marked read until you pass its `batch` back with `-Ack`.
- Claude Code: mail you have not read arrives as hidden context at your turn. When your session is idle, a turn may start with the line `mux-agents mail is waiting (wake <nonce>): run mux-agent read`. The wake turn carries no mail: run `read`, quote the nonce in your reply, act on the mail, then ack its batch with `read -Ack <batch>`. Mail you do not ack stays unread and wakes you again. When your task is done, run `report done`, then start `read -Wait 600` as a background command before your final answer. When the background `read -Wait 600` ends with `"timeout": true`, start it again in the background; when it returns mail, handle it, ack its batch with `read -Ack <batch>`, and start it again.
- Copilot CLI: at the start of each turn run `read -Deliver -Ack <batch>` (the batch of your last read, if any). When your task is done, run `report done`, then make `read -Wait 600 -Ack <batch>` your last tool call. On `"timeout": true` run it again; when it returns mail, handle it and listen again.
- Your client's hooks run `report -Session` at session start. Never run it yourself.
- Run each `send` as a command of its own, with `-BodyFile` and plain words.
- A line `message <id> ... run mux-agent read -Id <id>` names a large message. `read -Id` serves it only after you ack the batch that carried it first: run `read -Ack <batch>`, then `read -Id <id>`, or both at once with `read -Id <id> -Ack <batch>`. Handle the body it returns, then ack that body's own batch with `read -Ack <body-batch>`. While one is open, another `read -Id` returns it again with `"note": "ack it first"`.
- An ack whose batch's references would push the held large messages past 1024 is refused: drain them with `read -Id <id>`, ack each one, then retry the ack.
- A resumed member is a new generation; the ack history kept for its superseded generation is dropped at the next ack, unless that old generation still owns a pending receipt.
- `resume` refuses while any process on this machine may hold that conversation: a process whose command line names it, a client this team did not start whose own hook last reported it (or whose report cannot be read), or a process that cannot be read and may be a client. A member's own client holds only the conversation its hooks last reported; another Claude Code or Copilot session that reported another conversation, or none, holds nothing. Your own client is exempt: it holds only a conversation its command line or its own newest report names. Each refusal names the process (pid and image) or member and the rule that held. Known limits: a member that switches conversation with `/resume` while no hook fires still holds only its recorded one; a process that cannot be read is judged by its image name; on a host whose `/proc` is mounted with `hidepid` (other than 0) or `subset=pid`, or has another mount over it, `resume` always refuses, naming `/proc`; a client without this plugin's hooks that switched into the conversation by hand is not seen.
- `read -Cancel` stops listening until new mail arrives. The leader never listens on call: it reads at its next turn or waits with `read -Wait`.
- A child that finishes its task keeps listening until its parent stops it.

## Output

- Every command prints one JSON object.
- Exit code 0 means ok, 1 means error, and 2 means refused or usage.
- On failure, `check` names the cause.

## Refusals

- `entry`: no usable `mux-agent` binary for this OS and architecture.
- `herdr`: the caller runs in a herdr terminal outside tmux and rmux (`HC_TERMINAL_ID` is set); use herdr's own skills.
- `views`: you are an agent whose team's views are down (its multiplexer server is gone): `focus`, `spawn`, `resume`, `sidebar`, `watch` and `placement` refuse until the person runs `up`; `send`, `read`, `report`, `status`, `stop` and `list` (without panes) still work.
- `sandbox`: the caller runs in a sandbox: `SANDBOX_RUNTIME=1`, a macOS sandbox, Linux `NoNewPrivs`, or a Windows token that is unreadable, AppContainer, restricted or below Medium integrity.
- `policy`: the caller's live policy is unreadable or is neither bypass nor allow-all. For Claude Code, `reason` says whether the transcript or the screen failed and why (for example `screen: no mode line`).
- `flags`: a flag outside the allowlist, or an unreadable command line, on any `claude` or `copilot` process from the agent up to the pane.
- `directory`: the caller's launch directory cannot be confirmed.
- `tagged`: a spawn, a side bar or `placement` from an agent pane outside a team, or a side bar or `placement` from a team member; only the person or the team's leader opens the side bar or chooses where agents open.
- `depth`: a team member's child would be deeper than 3.
- `cap`: 10 live agents in the session.
- `limit`: the team already has 10 members besides the leader. The message names each member made before v0.3.0 that holds a slot: `mux-agent stop <name>` frees one whose shell is lost or ended (nothing is killed) and stops one that runs; one hc cannot show stays held.
- `name`: a bad or taken name.
- Other check names: `brief`, `target`, `parent`, `endpoint`, `state`, `lock`, `caller`, `launch`, `rmux`, `sidebar`, `usage` and `internal`.

## Shell shortcuts

For the person at a terminal, not for agents: `cc` starts Claude Code and `gg` starts Copilot CLI through mux-agents, passing every argument on. `<plugin folder>` is the plugin's real folder, two folders above this file's folder. Add one line to a profile:

- bash or zsh, in `~/.bashrc` or `~/.zshrc`: `. "<plugin folder>/scripts/shell/mux-agents.sh"`
- PowerShell, in `$PROFILE` (the primary shell for the shortcuts on Windows): `. "<plugin folder>/scripts/shell/mux-agents.ps1"`

- A symlinked or copied shortcut file cannot find the plugin: set `MUX_AGENTS_HOME` to the plugin folder before the line.
- `cc` shadows the C compiler `cc` in that shell.
- In `$PROFILE`, `cc` and `gg` refuse the argument `--%`, and the exit code is in `$LASTEXITCODE`; check it rather than `$?`.
- Under Git Bash, MSYS rewrites POSIX-looking paths in the arguments (`/tmp/f` becomes `C:/…/f`), as it does for any native program.
- The shortcuts never change `claude` or `copilot`: type `claude` or `copilot` for the bare client.
