# Module Reference

[简体中文](Module-Reference-zh-CN)

The map of the source tree: what each module owns and where to start reading it. [Architecture](Architecture) explains how the parts fit together; this page names the files. Each module opens with a `//!` header that states its job, and this page follows those headers.

## The crate

mux-agents is one Rust crate, `mux-agent` (`Cargo.toml`), edition 2024. It builds the `mux-agent` binary from `src/main.rs`, and the live tests' stand-in client `fake-client` from `src/bin/fake-client.rs`, which needs the `live` feature. Features: `live` (the live tests, the fake client and the spawn audit log) and `test-panic` (a panic at a chosen point, for `tests/panic.rs`). Dependencies: `serde_json`, `sha2` and `unicode-width` everywhere, `libc` on Unix and `windows-sys` on Windows.

`src/lib.rs` declares the top-level modules; a host other than Windows, Linux or macOS fails to compile. `src/main.rs` builds the `Ctx` and hands the arguments to `hold::main`, `cli::run_launcher` or `cli::run`.

## Top-level modules

| Module | Owns | Entry points |
|---|---|---|
| `cli` | The verb table, the argument parser and the dispatcher | `VERBS`, `parse`, `run`, `launcher_spec`, `run_launcher` |
| `ctx` | The providers every verb runs against: environment, identity, paths, and the OS, multiplexer, hc and clock seams | `Ctx`, `Env`, `Clock`, `SystemClock` |
| `error` | Typed refusals: one exit code, one check name and a fixed message | `Refusal`, `refuse`, `Res` |
| `json` | The reply on stdout: one compact ASCII JSON line, or `-Text` lines | `to_stdout`, `to_text`, `lines` |
| `caller` | The caller: its pane, its server's identity and its nearest Claude or Copilot client; `caller::hc` (file `caller_hc.rs`) resolves a caller in an hc terminal outside tmux and rmux | `resolve`, `resolve_tree`, `resolve_person`, `Caller` |
| `checks` | Spawn checks: the agent name, the context plan, and for an agent caller the flags, policy and directory checks | `assert_spawn_allowed`, `resolve_name`, `context_plan` |
| `sandbox` | The `sandbox` check: refuses a caller running inside a sandbox | `assert_unsandboxed`, `decide` |
| `mux` | The multiplexer seam: `<mux> -S <socket> -N <argv>` with the socket and binary resolved; the tmux and rmux version gate | `Mux`, `RealMux`, `endpoint`, `gate`, `native` |
| `hc` | The herdr-core transport; every call goes through the bounded process capture and hc's version is checked | `Hc`, `connect`, `Core` (`run`, `get`, `ls`, `kill`, `read`, `send`, `label`), `Lazy` |
| `spawn` | The only place a child process starts: captured calls with a timeout, and the attached client | `captured`, `attached`, `attached_env`, `Out` |
| `proc` | The process table seam; `proc/linux.rs`, `proc/macos.rs` and `proc/windows.rs` each supply an `Os`, `proc/seam.rs` holds open and kill, `proc/unix.rs` what Linux and macOS share | `Os`, `host`, `seam::open_with`, `seam::kill_with` |
| `parse` | Pure parsers of what the kernels and command lines hand back, tested on every host | `linux_stat`, `linux_cmdline`, `procargs2`, `vnode_cwd` |
| `launcher` | The pane launcher: waits for the launch commit, runs the client once, records its exit code | `launcher`, `SCRUB` |
| `launcher_scope` | The exact `launcher <spec> --state-root <abs> --path <value>` form and the environment it runs with | `argv`, `spec`, `environment` |
| `hold` | `mux-agent hold <release file>`: the first window of a session `up` creates | `main`, `wait` |
| `state` | The state folder: its root, privacy checks, locks and files (see below) | `state::root::root`, `state::lock::with_lock` |
| `sidebar` | `sidebar [-Close]` opens or closes one bar per session running `watch`, and `watch` draws the board until its pane closes | `sidebar`, `watch`, `ensure`, `run_watch`, `bar_width` |
| `paint` | Terminal text for the side bar and `list -Text`: color switches, the 16 named ANSI colors, display columns | `color_on`, `ascii`, `paint`, `hue` |
| `util` | Hashes, random ids and the `createdAt` time text | `sha256_hex`, `hex_id`, `guid`, `utc_o`, `parse_utc` |
| `testing` | Test only: recording fakes for the `Ctx` seams | `Fakes`, `TempDir`, `row` |

### `state`

| Module | Owns | Entry points |
|---|---|---|
| `state::root` | The state root from the environment | `root`, `HOST`, `OsKind` |
| `state::fs` | Atomic JSON writes and bounded reads; the size limits `STATE_MAX`, `TASK_MAX`, `BRIEF_MAX`, `BODY_MAX` | `write_json`, `write_json_durable`, `read_json` |
| `state::evidence` | The one reader and the one create-once writer of generation evidence and markers | `read`, `publish`, `unreadable` |
| `state::lock` | Per-user locks: `File::try_lock` on a private lock file on Unix, a named mutex on Windows; `LOCK_WAIT` | `with_lock`, `try_with_lock`, `admission` |
| `state::live` | The live-session registry under `live/`, one entry per client process | `report`, `lookup`, `alive` |
| `state::private_unix`, `state::private_windows`, `state::acl` | State folders private to the user, checked in-process; extended ACLs that could open them | `private_dirs`, `decide_acl`, `posix_acl_extended` |

### `sidebar`

| Module | Owns | Entry points |
|---|---|---|
| `sidebar::input` | The bar's own terminal: raw mode and mouse reporting while `watch` runs, restored on every exit | `open` |
| `sidebar::keys` | Terminal input as events: keys and SGR mouse reports | `Decoder`, `Event` |
| `sidebar::pick` | What a click or a key picks: the agent on the clicked row, and the keyboard selection | `row_hit`, `step`, `step_live` |
| `sidebar::tree` | A team's bar as a tree, leader first, each member's children under it | `is_team`, `walk`, `lay_team` |

## `src/agents`

`src/agents/mod.rs` holds the shared helpers: the per-server state folder (`state_dir`), the lock wrappers (`lock`, `try_lock`, `admit`), lock keys, and the id, pane and name validators. The modules below are grouped by job.

### Verbs and views

| Module | Owns | Entry points |
|---|---|---|
| `spawn` | `spawn`: authenticates the caller, connects hc, then admits and launches under the session's spawn lock | `spawn` |
| `up` (folder) | `up`, a person's verb that creates or re-enters a folder's team: discovery, the team index under `up/`, publication of the session, re-entry, retirement of a stopped team | `up::up`, `up::index`, `up::reenter`, `up::retire` |
| `leader` | The leader `up` launches, at depth 0 | `prepare`, `resume_ended` |
| `view` | `list`, `focus`, `status` and `report` | `list`, `focus`, `status`, `report` |
| `message` | `send`, `read`, `peek`, `notify` and `stop`; `drop` and `stop -Generation` in `message/` | `send`, `read`, `peek`, `stop_hc`, `drop_member`, `notify` |
| `resume` | `resume`: an ended member starts again with its conversation as a new member; `resume/recovery.rs` retries one that needs recovery | `resume` |
| `reconcile` | `restore`, and the pass `up` runs: every lifecycle member of a team reconciled, a Lost one brought back as a new generation | `restore`, `run`, `classify`, `observe` |
| `kept` | Kept shells: only their hc labels; lists and closes them | `kept`, `list`, `close` |
| `placement` | `placement` with `tab`, `h` or `v`, kept as the session option `@mux-agents-placement` | `placement`, `read_placement` |
| `split` | Where a split view goes, so the main chat keeps the most room | `plan`, `even_out`, `keep_main_half` |
| `attachment` | Opens a view pane running `hc attach` for a member, and cleans up one that failed | `attach`, `clean` |
| `board`, `board_rows` | The side-bar feed: hc membership with attached views and terminal history; one row per member | `board`, `mark_seen`, `hc_row` |
| `session_report` | `report -Session`: a client's hook names its conversation; runs the readiness handshake | `run`, `read_input` |

### Identity and membership

| Module | Owns | Entry points |
|---|---|---|
| `core_identity` | A member's hc identity: generation and hc name shapes, the presentation key, how a view's start command is read | `Identity`, `is_hc_name`, `attach_program`, `Key` |
| `core_facts` | One snapshot of the team's hc terminals, by their `mux.*` labels | `take`, `Snapshot`, `Observation` |
| `membership` | Who is a member now: stored records joined with hc facts, slots, names, superseded members | `Snapshot::take`, `locals`, `proven_self` |
| `member_target` | What a name or id names for `stop`, `drop` and the bar | `named`, `StopTarget` |
| `ident` | One validator per identity kind read from evidence; a malformed one never matches | `hc_name`, `hc_address`, `member_id`, `same` |
| `registry` | The session's agent panes, their records and exit notices; the conditional pane kills | `Snapshot::take`, `reap`, `reap_hc`, `add_notice`, `kill_if_dead` |
| `scope` | The one place a caller becomes a state scope: state follows the anchor, the multiplexer stays the caller's | `state`, `shown`, `team` |
| `tree` | A team's tree: whether the caller is in a team, where it stands, where its child goes, whom it may stop | `scope`, `check_depth`, `limit`, `MAX_DEPTH`, `MAX_MEMBERS` |
| `team` | A team's record and its permission ceiling, from the person's own client flags | `create`, `read`, `policy_of` |
| `ancestry` | The one walk from a process to the Claude or Copilot client above it | `first_client`, `identity` |
| `prelife` | Members made by 0.2.x or a v0.3 dev build: listed and stopped, never read past their record | `pre_lifecycle`, `lifecycle_only`, `close_pane` |

### Launch and generations

| Module | Owns | Entry points |
|---|---|---|
| `core_launch` | One launch of a member under the team lock: spec, `launch.json`, `hc run`, address, commit claim, promotion, view | `forward`, `Launch` |
| `gens` | The one reader of generation evidence under `gens/` | `member_of`, `members`, `publish_once`, `committed`, `next`, `Gen` |
| `team_state` | The one writer of `record.json`, `desired.json` and `view.json`, inside a team transaction | `open`, `held`, `Tx`, `view` |
| `lifecycle` | Classifies validated hc terminal facts, independent of any view | `classify` |
| `settle` | The replay table, the orphan fence, and ending one generation | `settle_gen`, `terminate`, `fence` |
| `release` | Ends a generation hc kept as a recovery shell by release, never by a kill | `release`, `complete`, `finish` |
| `outcome` | Each generation's one outcome, and the one predicate every surface shares; the only writer of `failure.json` and `outcome.json` | `adjudicate`, `needs_recovery` |
| `options` | Replay-safe client options: a launch argv split into options, session selectors and prompt | `classify`, `replay` |
| `copilot` | A Copilot fork's session copy | `copy_session` |
| `budget` | A team's spawn budget, a token bucket spent under the team lock | `check`, `spend` |

### Stop, cascade and adoption

| Module | Owns | Entry points |
|---|---|---|
| `cascade`, `cascade_journal`, `cascade_tree` | A stop records `stopped` for a member's whole sub-tree, closes it, then ends it bottom-up; the journal lets the next stop, `up` or bar tick finish one cut short; one snapshot of the session's tree | `prepare`, `execute`, `finish`, `Tree`, `Shown` |
| `adopt`, `adopt_bar`, `adopt_route` | When a member ends on its own, its nearest surviving ancestor adopts its children; the bar's sweep; reports routed to the logical parent | `publish`, `apply`, `tick`, `route` |

### Messaging and notebook

| Module | Owns | Entry points |
|---|---|---|
| `mail` | Mailboxes under `mail/`, one JSON message per line, read through the receipt cursor | `add_mail`, `read_mail`, `serve`, `ack`, `counts` |
| `receipt` | Mailbox receipt cursors, read and committed whole under the mailbox lock; shared with the notebook | `load`, `commit`, `settle` |
| `team_mail` | Team messaging: who a target names, one durable intent per send, delivery to each mailbox | `route`, `target`, `pick`, `write_intent`, `charge` |
| `notebook` | The team notebook: append-only `notebook.jsonl` under the team folder; `notebook/news.rs` writes the notebook block of each launch's first message | `note`, `notes`, `append`, `entries` |

### Occupancy

| Module | Owns | Entry points |
|---|---|---|
| `occupancy`, `occupancy_caller`, `occupancy_generations`, `occupancy_table` | Which live processes may hold a conversation; anything unreadable holds. The caller's own client, what each member's open generations claim, and the rules for one process-table row | `held`, `held_on`, `claims`, `occupies` |

## Tests

| Kind | Where | Convention |
|---|---|---|
| Unit | Inside the module or beside it | An inline `#[cfg(test)] mod tests`, a sibling `<module>_tests.rs`, or `<module>/tests.rs`, each declared `#[cfg(test)]` by its parent |
| Fixtures | `src/testing.rs`, `src/agents/mod.rs` (`fixture`), `src/agents/*_fixture.rs` | Recording fakes for every `Ctx` seam and a `World` with a fake pane, client and private state root; compiled only into tests |
| CLI | `tests/cli.rs`, `tests/panic.rs` | The built binary end to end: argv in, one reply line and an exit code out |
| Native | `tests/occupancy_native.rs`, `tests/live_session.rs`, `tests/liveness_windows.rs` | The real process table of the host |
| Live | `tests/live.rs`, `tests/live_windows.rs`, `tests/support/` | Real tmux 3.8, tmux 3.2a and rmux servers on private sockets with `fake-client`, behind the `live` feature; every child they start is bounded (`tests/support/bounded.rs`) |
| Repository guards | `tests/repo.rs` | Rules checked over the source text |
| Shell and hooks | `tests/*.sh`, `tests/shim.cmd`, `tests/claude/` | The shims, release, wiki publisher and upgrade; the Claude Code hooks module |

A few of the guards in `tests/repo.rs`:

- `process_spawn_only_in_spawn_rs`: no module but `src/spawn.rs` starts a process.
- `state_reads_go_through_scope`: state reads in `src/agents` and `src/sidebar.rs` go through `scope::state`.
- `only_team_state_writes_member_records`: only `src/agents/team_state.rs` writes member files.
- `generation_evidence_is_read_only_through_gens`: the launcher and member proof read evidence only through `gens`.
- `every_test_file_is_exempt_by_name`: every `*_tests.rs` and `tests.rs` file is declared test only.
- `wiki_pairs_and_links`, `zh_twins_match_line_counts`, `no_issue_or_pull_request_numbers`: the wiki rules.

[Development and Release](Development-and-Release) has the commands that run them.
