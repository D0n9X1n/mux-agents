# Contributing

One pull request per issue, on its own branch.

Commits are authored and committed as `D0n9X1n <D0n9x1n@outlook.com>`. A pull request lands by fast-forwarding `main` to its tested head, never through a GitHub merge, which would re-author it.

## Layout

mux-agents is one Rust crate that builds one binary, `mux-agent`. The plugin runs it through a shim: `scripts/mux-agent` (POSIX sh) or `scripts\mux-agent.cmd` (Windows). The shim runs the first usable of `MUX_AGENTS_BIN`, `scripts/bin/<os>-<arch>/mux-agent[.exe]` (os `macos`, `linux` or `windows`; arch `x86_64` or `aarch64`) and `target/release/mux-agent[.exe]`; with none it prints a JSON `entry` refusal and exits 1. A checkout of `main` therefore runs after `cargo build --release`.

| Path | Job |
|---|---|
| `src/` | The binary: verbs, checks, state, locks, the process layer per OS, the launcher and `watch` |
| `scripts/mux-agent`, `scripts/mux-agent.cmd` | Entry shims |
| `scripts/release/` | `package.sh` builds and checksums the binaries; `gate.sh` decides whether a tag may be released and whether `dist` may move |
| `scripts/wiki/` | The publisher the Publish wiki workflow runs |
| `scripts/dev/` | Developer tools: `bench-list.sh` times `list`, `tmux32.sh` builds tmux 3.2a for the live tests |
| `hooks/`, `types/` | The Claude Code `Agents` pane and its state contract |
| `tests/` | `repo.rs` (repository rules), `cli.rs`, `panic.rs`, `live.rs` (real tmux), the shim, release and wiki sh tests, and `tests/claude/` (the pane) |

## Must-pass

CI runs these on every pull request; run them before you open one. Rust 1.95.0, as CI pins it.

On Linux, macOS and Windows (`test`):

```sh
cargo fmt --check
cargo clippy --all-targets --features test-panic --locked -- -D warnings
cargo test --features test-panic --locked
cargo build --release --locked
cargo test --release --features test-panic --locked --test panic
sh tests/shim.sh
```

On Windows, `tests\shim.cmd` replaces `sh tests/shim.sh`.

On Linux and macOS with tmux (`integration`); `tmux32.sh` builds tmux 3.2a and prints `MUX_AGENTS_TMUX32=<path>`, which the live tests read:

```sh
sh scripts/dev/tmux32.sh
cargo test --features live,test-panic --locked --test live
sh tests/release.sh
sh tests/wiki.sh
sh tests/upgrade.sh   # real Claude Code (and Copilot CLI when on PATH), isolated in a temp dir, git fetches local
```

Lint (`lint`), with actionlint 1.7.12, shellcheck and Claude Code 2.1.310 or later:

```sh
actionlint
git ls-files -z 'scripts/*.sh' scripts/mux-agent 'tests/*.sh' | xargs -0 shellcheck
claude plugin validate .
claude plugin test .
```

## Documentation

`wiki/` is the source of truth for documentation; the repository has no `docs/` folder. Every page is written in English and Chinese, as `Page.md` and `Page-zh-CN.md`, and links to its twin at the top. The two have the same number of lines, line for line. Link pages by page name, as in `[Home](Home)`. The Publish wiki workflow mirrors `wiki/*.md` to the GitHub wiki on every push to `main`.

## Merge procedure

1. `gh pr view <n> --json headRefOid,statusCheckRollup`
2. Every check on that `headRefOid` is `SUCCESS`, and `lint`, `test (ubuntu-latest)`, `test (macos-latest)`, `test (windows-latest)`, `integration (ubuntu-latest)` and `integration (macos-latest)` are all present.
3. `git fetch origin <sha>`, then `git push origin <sha>:main`, with `<sha>` the `headRefOid` from step 1. The push is not forced, so it lands only when `<sha>` builds on the current `main`; GitHub then marks the pull request merged.

## Releases

`main` is source only. A release builds six binaries (macOS, Linux and Windows, each `x86_64` and `aarch64`) into `scripts/bin/` with one `SHA256SUMS`, and commits them on the `dist` branch, which the marketplace installs.

1. On `main`, set the same version in `.claude-plugin/plugin.json` and `Cargo.toml` (then `cargo build` to update `Cargo.lock`), and add the release page `wiki/Release-v<version>.md` and its twin.
2. Push tag `v<version>` on that commit once its CI push run is green. The release workflow refuses a tag that is not on `main`, has no green CI push run, or differs from the `plugin.json` or `Cargo.toml` version, and moves `dist` only forward.
