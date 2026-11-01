# mux-agents v0.2.2 release notes

[简体中文](https://github.com/D0n9X1n/mux-agents/wiki/Release-v0.2.2-zh-CN)

Start Claude Code and Copilot CLI agents in tmux or rmux panes, see what each is doing, message them and stop them.

## What changed

- **A side bar you can read at a glance.** Each live agent shows its state (`◉ working`, `◌ waiting`), its name in its own color, its `[model]` and `✉ N` for unread mail; ended agents follow as `✓ done`, `✗ exit N` or `- stopped`. Rows are laid out at the pane's real width and height and redrawn in place, so the bar no longer flickers. `NO_COLOR`, `FORCE_COLOR` and `MUX_AGENTS_ASCII=1` are honoured. ([#37](https://github.com/D0n9X1n/mux-agents/issues/37))
- **The side bar stays on the right edge**, full height, `max(40, window width / 5)` columns, and is put back after a split, a layout change, `swap-pane` or `rotate-window`. ([#36](https://github.com/D0n9X1n/mux-agents/issues/36))
- **`sidebar -Close` can no longer close your own pane.** The bar is the process `sidebar` started, proven by its pane, pid and start time, never by its label. ([#47](https://github.com/D0n9X1n/mux-agents/issues/47))
- **Agents are named after their task** (`add-subtract-function`), the name is shown as the tab's name and the pane's title, and a name is never reused in its session. ([#39](https://github.com/D0n9X1n/mux-agents/issues/39))
- **Lost agents close on their own.** An agent whose tab or pane is gone is closed with one notice to its parent and leaves the side bar; `stop` on an ended agent answers `already`. ([#38](https://github.com/D0n9X1n/mux-agents/issues/38))
- **`spawn -Model <model>`** chooses the agent's model; `list`, the side bar and the Agents pane show it. ([#41](https://github.com/D0n9X1n/mux-agents/issues/41))
- **No false `policy` refusals** for a Claude Code caller in bypass mode whose footer sits a few rows up or wraps; every `policy` refusal names its reason. ([#40](https://github.com/D0n9X1n/mux-agents/issues/40))
- **Terminal CLI only.** The Claude Code desktop app, web, IDE panels and headless runs get no Agents pane, no `/mux-agents-pane` and no polling. ([#43](https://github.com/D0n9X1n/mux-agents/issues/43))

## Install

```text
claude plugin marketplace add D0n9X1n/mux-agents
claude plugin install mux-agents@mux-agents
copilot plugin marketplace add D0n9X1n/mux-agents
copilot plugin install mux-agents@mux-agents
```

## Upgrade

From 0.2.0 or 0.2.1, close the side bar first with `sidebar -Close`, update with one line per client and restart your sessions:

```text
claude plugin marketplace update mux-agents && claude plugin update mux-agents@mux-agents
copilot plugin marketplace update mux-agents && copilot plugin update mux-agents@mux-agents
```

A side bar left open by 0.2.1 has no record of its process, so 0.2.2 never closes it: `sidebar` refuses and names it; close it with `kill-pane -t <pane>`. The state folder and its record, mail and lock formats are unchanged within 0.2.x; records gain `model` and `color`, which older records simply lack. Agent panes keep the binary they started with until you restart them; `stop` them first if you want them on 0.2.2.

## Known limits

- The side bar's right-edge lock is tested live on tmux 3.8, tmux 3.2a and rmux 0.10 on macOS; native Windows rmux is untested.
- rmux 0.10's `rotate-window` leaves agent tags on the wrong panes, so `list` and `read` can name the wrong pane after a rotate ([#48](https://github.com/D0n9X1n/mux-agents/issues/48)).
- A manual check that the Claude Code desktop app shows no Agents pane is still pending ([#43](https://github.com/D0n9X1n/mux-agents/issues/43)).
