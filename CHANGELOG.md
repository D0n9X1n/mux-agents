# Changelog

All notable changes to mux-agents. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/). Full notes are on the wiki.

## [Unreleased]

### Added

- `spawn -Model <model>` chooses the new agent's model (a name or alias such as `sonnet` or `sonnet[1m]`); it reaches the client as `--model <model>`. The spawn reply names `model` after `reason` (`inherited` without `-Model`), `record.json` keeps it, `list` rows carry it (null when inherited), and the side bar and the Claude Code `Agents` pane show `[<model>]` after a live agent's client. ([#41](https://github.com/D0n9X1n/mux-agents/issues/41))

### Changed

- Agents get names that say what they do, never reused in their session, shown as the tab's name or the pane's title (`select-pane -T`, focus unchanged). Without `-Name` the name comes from the task's first words (stopwords dropped, up to 3 words within 24), so "Please add a subtract function to calc.py" becomes `add-subtract-function`; a reserved or empty result falls back to `<client>-N`. Names taken by live agents or by any recorded agent of the session in this state root, ended ones included, are skipped with `-2`, `-3`, … cut at a word boundary; a taken `-Name` is refused `name` with a free one to try. The skill now always passes `-Name <short-kebab-name>`. ([#39](https://github.com/D0n9X1n/mux-agents/issues/39))

### Fixed

- A Claude Code caller in bypass mode is no longer refused `policy` when its footer sits a few rows up or wraps: the pane is captured with `-J`, the last 8 non-blank rows are searched, only a documented footer badge counts as a mode line and the lowest one decides. A transcript or screen that shows no mode yet is read again, up to 3 times 250 ms apart. Every Claude Code `policy` refusal now names its reason, in its message and as `reason`. ([#40](https://github.com/D0n9X1n/mux-agents/issues/40))
- An agent whose tab or pane is gone no longer stays in the side bar as `lost`: the next `list`, `read`, `spawn`, `stop` or side-bar tick closes it with exactly one notice to its parent, removes it from `list` and the bar, and keeps it in `list -All` with `closedBy: "pane"`. A pane `remain-on-exit` kept after its agent ended is closed by one server-side check that it is still dead and still tagged as that agent; a pid is never signalled. An agent that never committed is closed only after 60 s, under the session's spawn lock. `stop` on an agent that already ended answers `ok` with `already: "<state>"` instead of a `target` refusal, and closes, signals or notices nothing. The exit message now carries `kind` and `exitCode`, so the first notice wins even when its marker write failed. ([#38](https://github.com/D0n9X1n/mux-agents/issues/38))

## [0.2.1] - 2026-11-01

Choose tabs or panes per session, and the side bar opens on its own. [Release notes](https://github.com/D0n9X1n/mux-agents/wiki/Release-v0.2.1).

### Added

- `placement tab|h|v` chooses, for the rest of the session, whether `spawn` opens agents as tabs (new windows) or as panes beside you; `placement` alone shows the choice. It is a multiplexer session option, so nothing is written to disk. `spawn -Tab` (new) or `-Split h|v` overrides it for one spawn, and the spawn reply names `placement` and `placementSource`.
- The side bar opens on its own: after each successful `spawn`, the session's bar is found or opened beside you. `sidebar -Close` keeps it closed for the session and `sidebar` opens it again. The spawn reply names it as `sidebar`; a side-bar failure never fails the spawn and is reported as `sidebarError`.

### Changed

- `sidebar -Close` and `sidebar` now also turn the automatic side bar off and on for the session.
- The live tests print each multiplexer's version first, and `MUX_AGENTS_LIVE_REQUIRE=tmux,tmux32,rmux` turns a skipped multiplexer into a failure.
- A shorter README with previews of the side bar, `list`, a mailbox report and a refused spawn; the details moved to the wiki's [Usage](https://github.com/D0n9X1n/mux-agents/wiki/Usage) page.
- Added `SECURITY.md`, `CODE_OF_CONDUCT.md` and this changelog.

## [0.2.0] - 2026-11-01

One native Rust binary replaces the PowerShell implementation: no PowerShell or .NET is needed on any OS. [Release notes](https://github.com/D0n9X1n/mux-agents/wiki/Release-v0.2.0).

### Added

- Prebuilt binaries for macOS, Linux and Windows on x86_64 and aarch64, shipped on the `dist` branch with `SHA256SUMS`; the marketplace installs them, so nothing is built or downloaded at run time.
- rmux 0.10+ on every OS; tmux 3.2+ on macOS and Linux.
- A one-line upgrade for Claude Code and Copilot CLI.

### Changed

- `list` with six agents takes about 23 ms.
- Unix locks are files under `~/.local/state/mux-agents/locks`.
- The lock wait counts wall-clock time, and a state folder another call creates at the same moment is checked instead of refused.

### Fixed

- The .NET named-mutex advisory [GHSA-2j93-m82w-73j2](https://github.com/advisories/GHSA-2j93-m82w-73j2) no longer applies: no .NET runs.

### Removed

- All PowerShell code and Pester suites.

## [0.1.0] - 2026-10-31

The first release: `spawn`, `list`, `status`, `send`, `read`, `focus`, `stop` and the side bar on Windows (rmux) and on Linux and macOS (tmux), written in PowerShell 7.4. [Release notes](https://github.com/D0n9X1n/mux-agents/wiki/Release-v0.1.0).

[Unreleased]: https://github.com/D0n9X1n/mux-agents/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/D0n9X1n/mux-agents/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/D0n9X1n/mux-agents/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/D0n9X1n/mux-agents/releases/tag/v0.1.0
