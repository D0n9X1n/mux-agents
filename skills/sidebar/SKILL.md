---
name: sidebar
description: "Terminal CLI only: needs a tmux or rmux pane. Open, close or toggle the mux-agents side bar in every agent window of this session. Run only when the person types /mux-agents:sidebar."
disable-model-invocation: true
argument-hint: "[open|close|toggle]"
---

The person typed this command to show or hide the mux-agents side bar in every window of this tmux or rmux session. Run exactly one command, then report its JSON reply in one line. Run nothing else.

`<entry>` is `scripts/mux-agent` in this plugin (`scripts/mux-agent.cmd` on Windows): two folders above this file's folder (`<skill folder>/../../scripts/mux-agent`). Use the absolute path with forward slashes. In PowerShell, prefix `& `.

The word the person typed: $ARGUMENTS

- No word, or `toggle`: `"<entry>" sidebar -Toggle`
- `open`: `"<entry>" sidebar`
- `close`: `"<entry>" sidebar -Close`
- Any other word: run nothing, and answer `usage: /mux-agents:sidebar [open|close|toggle]`.

Report the reply in one line:

- `"opened": true`: `Side bar opened: <pane>. Every agent window gets its own.`
- `"opened": false`: `Side bar is on: <pane>. Every agent window gets its own.`
- `"closed"` with panes: `Side bar closed: <panes>.`; with none: `Side bar was already off.`
- `"ok": false`: `mux-agents: <message>`.
