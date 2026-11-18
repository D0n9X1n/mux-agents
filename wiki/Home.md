# mux-agents Wiki

[简体中文](Home-zh-CN)

mux-agents is a plugin for Claude Code and GitHub Copilot CLI. From a client running in a tmux or rmux pane, it starts a team of Claude Code and Copilot CLI agents in panes you can see, lets them message each other through files, and keeps every agent running when its pane closes, the multiplexer server dies or the machine reboots.
It is one native Rust binary, `mux-agent`, on macOS, Linux and Windows. **Start with [Usage](Usage)** to install it and run your first team.

### User guide

- [Usage](Usage) — install, commands, safety checks, requirements, upgrades and troubleshooting
- [Teams and Lifecycle](Teams-and-Lifecycle) — start a team with `up`, spawn, stop, resume, recover, and bring a team back after a crash or reboot
- [Side Bar](Side-Bar) — what the bar shows, its keys and mouse, and how it is drawn
- [Messaging and Notebook](Messaging-and-Notebook) — mail between agents, how it reaches each client, and the team notebook

### How mux-agents works

Read [Architecture](Architecture) for the map, then
[From Spawn to Pane](From-Spawn-to-Pane) for one agent's round trip.
Use the references below for a particular subsystem.

- [Architecture](Architecture) — the problem, the moving parts, and the design rules
- [From Spawn to Pane](From-Spawn-to-Pane) — one `spawn`, from the request to a running agent in its pane
- [Agent Lifecycle](Agent-Lifecycle) — generations, stop, resume, recover, restore, and `up` re-entry
- [State and Durability](State-and-Durability) — the state root, create-once publication, locks, and fail-closed reads
- [Safety and Occupancy](Safety-and-Occupancy) — spawn checks, process identity, and what may hold a conversation
- [Module Reference](Module-Reference) — every source module and what it owns

### Build and contribute

- [Development and Release](Development-and-Release) — layout, must-pass checks, live tests, merging, releases, and wiki publication

### Design history

These pages record the v0.1.0 and 0.2.0 designs. The pages above describe the current system.

- [Foundation](Design-Foundation) — the rename to mux-agents, CI on three operating systems, and the tag-driven release
- [Cross-platform](Design-Cross-Platform) — tmux on Linux and macOS, rmux everywhere, with the same checks
- [Side bar](Design-Side-Bar) — the first side bar: the agent count and what each agent is doing
- [Runtime check](Design-Runtime-Check) — refusing an affected .NET, dropped in 0.2.0 when .NET went away
