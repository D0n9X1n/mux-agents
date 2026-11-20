# Safety and Occupancy

[简体中文](Safety-and-Occupancy-zh-CN)

mux-agents lets agents start, message and stop other agents with no person watching each step. This page lists the rules that stop it from doing harm: who may spawn and with what, how a process is proven to be a member, what must be true before a stopped conversation is resumed, and who may act on whom. Every rule reads the state described in [State and Durability](State-and-Durability), and every rule fails closed: when the evidence cannot be read, the answer is the safe one, never the convenient one.

## Threat model

In scope:

- **Crashes and power loss** at any step of a launch, a stop or `up`. Each step is a durable write followed by a crash point, and the next command settles what is left.
- **Reboots and server restarts.** A server is named by its socket, pid and start, so a restarted server is a new key. Linux start times count from boot, so the live-session registry records the boot id and an entry from another boot counts as dead (`src/state/live.rs`).
- **Torn or odd files.** A torn JSON file, a FIFO, a link or a file where a folder should be reads as `Unreadable`, and `Unreadable` never counts as free or absent.
- **Other generations.** Every launch is a new generation with its own create-once evidence, and each write is guarded by the generation it belongs to, so an old launch cannot overwrite a new one.
- **PID reuse.** A process is named by its pid and its start, never its pid alone. A walk ends at a parent that started after its child; a row is read with its start read again last; a kill goes through a handle opened before the start is read, so it cannot reach a reused pid (`src/proc/seam.rs`).
- **Other users on the machine.** State folders are private to the user and locks are per user ([State and Durability](State-and-Durability)).

Out of scope: a hostile process running as the same user. It can already read and write the user's files and signal the user's processes, so no check inside mux-agents could stop it. The rules here guard against accidents, races and other users.

## Spawn checks

Each refusal names its check (`code`, `check`, `message`); the user-facing table is in [Usage](Usage).

| Check | Refuses | Why |
|---|---|---|
| `sandbox` | the caller runs in a sandbox: `SANDBOX_RUNTIME=1`; Linux `NoNewPrivs` set or unreadable; macOS `sandbox_check` non-zero; a Windows token that is unreadable, AppContainer, restricted or below Medium integrity | a spawned agent runs in a new multiplexer terminal, not inside the caller's sandbox, so a sandboxed caller would otherwise reach outside it |
| `tagged` | outside a team, a caller whose pane carries an agent tag or is an agent's own pane; a herdr terminal whose member proof failed | outside a team only the top-level chat spawns, so agents cannot fan out with no tree to bound them; a failed proof never falls back to pane identity |
| `flags` | any `claude` or `copilot` process from the agent up to the pane with a flag outside the allowlist, or whose command line cannot be read | the policy and session checks only understand the allowlisted flags; a flag they cannot judge, or a command line they cannot see, is refused rather than guessed |
| `policy` | the caller's live policy is not full bypass | a child starts with full permissions, so only a caller that already has them may start one |
| `directory` | the caller's launch directory cannot be confirmed, or is not an existing absolute folder | children start where the caller started; a relative path would resolve against `mux-agent`'s own working directory, not the agent's |
| `cap` | outside a team, 10 agents already live in the session | bounds a chat that has no tree |
| `depth` | in a team, a child deeper than 3 (the leader is 0) | bounds how far a team can fan out |
| `limit` | in a team, 10 members already besides the leader; a held pre-lifecycle folder counts | bounds a team's size, and an unclassified folder keeps its slot |
| `ceiling` | in a team, a child's flags above its parent's policy or the team's permission ceiling | a child never gets more permission than its parent or the team |
| `budget` | in a team, a member's spawn when the team's token bucket is empty | bounds how fast members can start agents |

Where each runs:

- `sandbox` runs on every public verb before any multiplexer call (`src/cli.rs`, `src/sandbox.rs`), not only on `spawn`. A launcher pane that mux-agents admitted passed it when it was spawned, so nothing is read again.
- Outside a team, an agent caller that is not admitted passes `flags`, `policy` and `directory` in that order (`checks::assert_spawn_allowed` in `src/checks.rs`), then `cap` and the name check (`src/agents/membership.rs`).
- In a team, `depth` is checked when the child is admitted, then `limit`, `ceiling`, the name and, for a member caller, `budget`, all under the team lock (`src/agents/tree.rs`). The budget is a token bucket in `teams/<sha16>.budget.json`, set by the session options `@mux-agents-spawn-budget` and `@mux-agents-spawn-refill` (`src/agents/budget.rs`); the launch takes its token as its last step before `hc run`, so a later refusal spends nothing.

How `policy` and `directory` read the caller:

- **Claude Code policy:** the latest `permission-mode` the session's transcript recorded, then the pane's lowest mode line among its last 8 non-blank rows, which must be the badge `⏵⏵ bypass permissions on`. The pane catches a Shift+Tab the transcript has not recorded yet. A missing mode is read again, up to 3 times 250 ms apart; the refusal's `reason` names the half that failed.
- **Copilot CLI policy:** the latest permissions event, or with none an allow-all flag at launch, and never plan mode.
- **Directory:** on Windows, Claude Code's `~/.claude/sessions/<pid>.json` must carry the agent's start and this session's id; on Unix, the agent's kernel cwd is read between two reads of its start, and a session file that is present must agree with it; Copilot CLI uses the first `cwd: ` line of its `workspace.yaml`.

## Identity

A process is a member because of evidence it cannot fake by its name: its pid and start, its hc terminal address and labels, and the exact command its pane started. A name only looks up an id in the records.

**A pane caller** (`caller::resolve` in `src/caller.rs`). The multiplexer probe gives the pane's pid, session, window and `@agent-id`, and the server pid it reports must be the endpoint's. The server's start is read through an open handle, and `<serverKey>` hashes socket, pid and start. Then the walk goes up from the caller's own process, one row at a time, to at most 64 parents, ending at the pane. A parent that started after its child is a reused pid and ends the walk. The nearest Claude Code or Copilot CLI process above the caller is the agent, and only shells may run in between (else 2 `caller`). The agent's start is read through an open handle; an unreadable or changed start refuses 1 `caller`.

**An hc member** (`src/caller_hc.rs`). This path applies when `HC_TERMINAL_ID` is set and neither `TMUX` nor `RMUX` is. The terminal's `mux.team` label must be `<16 hex>:$<n>` and its `mux.id` an agent id. Every file is read through `within_root`, only where its canonical path is the state root's own, so no link leads out. The member's active generation must be committed, its record must pass `Identity::read`, its `hcAddress` must equal `HC_TERMINAL_ID`, the terminal must still show its launch (`tree::launch_holds`), and `server.json` must hash to the key. Then `tree::prove_member` checks the process chain: the caller, then only shells, then the client that `launcher.json` recorded (on Unix also the native `copilot` whose parent is that client), whose parent is the recorded launcher; every pid and start must match, and the terminal must show one of them in front. hc names a terminal's foreground only once it has sampled it, so `tree::sampled` reads it again up to `SAMPLE_READS` (20) times before the proof decides.

A member whose proof fails is a `candidate`, never a `member`. Every verb but the tree verbs refuses it 2 `tagged`; the tree verbs prove it again under the team lock (`tree::standing`), and a failure there is 2 `tagged` too, never a fallback to pane identity. A herdr terminal that is no mux member at all refuses 2 `herdr`.

**Identity values** (`src/agents/ident.rs`). An hc name is `m` and 8 lowercase hex digits, an hc address is non-empty with no whitespace or control character, and a member id is 12 lowercase hex digits. `ident::same` matches only when both sides are present, both valid and equal, so two empty or missing values never match. A recorded identity that fails its validator refuses 1 `evidence` and does nothing.

**Views** (`src/agents/core_identity.rs`). A pane is a member's view only when its start command is exactly `<program> attach <address>` for that member's address, with an hc install name as the program (`core_identity::attach_program`). `send`, `read`, `focus` and `stop` find an agent by its pane and that exact command, never by `@agent-*` tags, so an rmux `rotate-window`, which leaves tags behind, cannot point them at the wrong pane.

## Occupancy

`resume` starts a client on an existing conversation. If another live client still writes to that conversation, two clients would write one transcript. So before a resume, `occupancy::held` asks whether any live process may hold the conversation (`src/agents/occupancy.rs`). Each answer names its rule:

| Rule | Holds when |
|---|---|
| `Table` | the process table cannot be read |
| `Hidden` | on Linux, `/proc` may hide other users' processes: more than one mount at `/proc`, one that is not `proc`, or `hidepid` other than `0` or `off`, or `subset=pid` |
| `Evidence` | a member's record, status, spec or registry entry cannot be read, or a process's report or identity cannot be resolved |
| `Managed` | a client a member's generation started, whose effective session is this conversation |
| `Argv` | a command line names this conversation |
| `Caller` | the caller's own client, whose newest registry entry names this conversation |
| `Unmanaged` | a client of the kind this team did not start, whose own newest report names this conversation |
| `Unbound` | a process that cannot be read and may be a client |
| `Reserved` | a generation that has not ended reserves its conversation and no live client is bound to it |

A command line names the conversation when its uuid follows `--session-id`, or `--resume` or `-r` without `--fork-session`. A Claude Code fork occupies only its new conversation; words after `--` are no flags (`occupancy_table::occupies`).

The order (`occupancy::held_on`): the `/proc` mounts, one table read, the claims of every member of every server key under the state root, the caller's own client, then each row of the table in turn. A managed client gone since the table read is named last, and a reservation only when nothing else holds.

What clears a process:

- **An exited or unreaped process holds nothing.** A client that exited but is not yet reaped is still listed, with its path and argv gone, so right after a `stop` it would read as `Unbound`. `Os::ended` answers for it (`src/proc/mod.rs`): Linux reads `/proc/<pid>/stat` for the same start and sees a zombie or a process in its exit; macOS reads `kinfo_proc`, because libproc answers nothing for a zombie; on Windows an exited process leaves the table. It runs no more code, so it is cleared.
- **A client this team did not start holds only on its own evidence.** Another Claude Code or Copilot CLI session open on the machine must not block every resume, so an unmanaged client of the kind holds only when its own newest report names this conversation, or when that report cannot be read (`occupancy_caller::unmanaged_report`). A report naming another conversation holds nothing, and so does a client that exited since the table read. The known limit is a client without this plugin's hooks that switched into the conversation by hand: it is not seen.
- **An image that names no client of the kind clears an unreadable row.** A Unix `node` without a path, whose executable was replaced on disk while it runs, is judged by its script, so an unrelated `node` does not hold every Copilot CLI conversation.
- **A managed client's effective session** is its newest report when that report's `at` is not earlier than `launcher.json`'s `startedAt`; an older one comes from another boot and is ignored, and the generation's own session holds.
- **An orphan with no `spec.json`** never had a client and reserves nothing.

What fails closed: an unreadable table or doubtful `/proc`, an unreadable member, the caller's own pid listed without a start, a managed client whose effective session is unknown, and an unmanaged report that cannot be read all hold. The holder is named as `process <pid> (<image>)`, `member <id>` or `this machine`, with the rule's phrase and "if you just stopped it, retry in a few seconds". `resume` refuses 2 `session` with that message, recovery refuses 2 `running`, and `up` answers `"held": "occupied"` with a `holder` for the leader.

## Caller authority

| Action | Who may |
|---|---|
| `stop`, `read <name>`, `resume` in a team | a person; a member only when its id is in the target's chain of ancestors up to the leader (else 2 `authority`) |
| `stop` outside a team | a person; an agent only when it is the target's recorded parent (else 2 `parent`) |
| `resume` outside a team | a person only (an agent is 2 `tagged`) |
| `spawn` | outside a team, the top-level chat; in a team, a person or a member, down the tree |
| side bar, `placement`, notices | a person, or in a team the leader for `placement` |

In a team, `tree::with_standing` holds the team lock across the authority check and the action, so the tree cannot change between them (`src/agents/tree.rs`). Ancestry comes from records only: mail grants nothing.

Messages go to mailboxes, never as keystrokes ([Messaging and Notebook](Messaging-and-Notebook)). A team `send` names `parent`, `leader`, `children`, `team`, a member or a pane, and is one durable intent under the team lock (`src/agents/team_mail.rs`). A member sender is charged against `QUOTA` (120 intents in an hour); past it the send refuses 2 `quota`. A reply that would be hop 4 refuses 2 `hop`, so two agents cannot answer each other forever. Agents pass free text only in files (`-TaskFile`, `-BriefFile`, `-BodyFile`), never on their command line.

## Source map

| Contract | Source |
|---|---|
| Sandbox | `src/sandbox.rs`, `src/cli.rs` |
| Flags, policy, directory, names | `src/checks.rs` |
| Session cap | `src/agents/membership.rs` |
| Tree: depth, limit, ceiling, authority, member proof | `src/agents/tree.rs` |
| Spawn budget | `src/agents/budget.rs` |
| Pane caller | `src/caller.rs` |
| hc member caller | `src/caller_hc.rs`, `src/caller_hc_tests.rs` |
| Identity validators, views | `src/agents/ident.rs`, `src/agents/core_identity.rs` |
| Occupancy | `src/agents/occupancy.rs`, `src/agents/occupancy_table.rs`, `src/agents/occupancy_caller.rs`, `src/agents/occupancy_generations.rs` |
| Process table, ended processes, kill | `src/proc/mod.rs`, `src/proc/seam.rs`, `src/proc/linux.rs`, `src/proc/macos.rs`, `src/proc/windows.rs` |
| Stop outside a team | `src/agents/message.rs` |
| Team messaging | `src/agents/team_mail.rs` |
