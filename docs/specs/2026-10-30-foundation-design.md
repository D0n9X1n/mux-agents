# Foundation for v0.1.0: rename, CI, release

Sub-project A of milestone v0.1.0: #3 rename, #6 `parentAgentId`, #2 CI, #5 release. Sub-projects B (#1), C (#4) and D (#7–#9) build on it.

## Purpose

Fix the name before anyone installs it, and give every later pull request CI on Linux, macOS and Windows plus a tag-driven release, following `D0n9X1n/SonicTerm`.

## Decisions

1. Clean rename to `mux-agents`, no aliases: nothing has been released.
2. Tests move to Pester 4.10.1. 3.4.0 does not import on the Linux and macOS runners (CI probe, 2026-10-30); 4.10.1 runs the current suite unchanged (179 passed, 0 failed on Windows).
3. Until sub-project B, the existing suites carry Pester tag `windows`, excluded off Windows. New suites run everywhere.
4. A release is a GitHub release with a plugin zip, published only for a tag on `main` with a green `CI` push run and a matching `plugin.json` version.
5. Add an MIT `LICENSE` (PM's call; reversible while private).

## Changes

**Rename (#3).** Files: `scripts/mux-agent.ps1`, `scripts/mux-agent-launcher.ps1`, `scripts/MuxAgents.ps1`, `skills/mux-agents/`, `tests/MuxAgents*.ps1`. Every `RmuxAgent` identifier becomes `MuxAgent`, test fakes included (`New-FakeMuxAgentProviders`); `rmux-agent` text becomes `mux-agent`; `RMUX_AGENTS_STATE` becomes `MUX_AGENTS_STATE`; state root `%LOCALAPPDATA%\mux-agents\state`; mutex prefix `Local\MuxAgents-`; manifests, `[mux-agent]` header, error prefixes and README follow. Names that mean the multiplexer stay: `RMUX`, `TMUX`, `TMUX_PANE`, `rmux.exe`, `Add-FakeRmuxRule`, the `-N` rule.

**`parentAgentId` (#6).** `Add-MuxAgentNotice` stops reading `Record.parentAgentId`, which nothing writes, and always posts to `pane-<n>` of `Record.parentPane`. Children cannot spawn, so a parent is never an agent pane.

**CI (#2).** `.github/workflows/ci.yml`: on `pull_request` and `push` to `main`; `contents: read`; a concurrency group cancels superseded pull-request runs; actions pinned by SHA with a version comment.
- `lint` (ubuntu): actionlint 1.7.12 verified against its pinned SHA-256; both manifests parse as JSON.
- `test`, matrix `ubuntu-latest`, `macos-latest`, `windows-latest`, `fail-fast: false`: install Pester 4.10.1; on Windows set `TEMP`/`TMP` to `RUNNER_TEMP` (the 8.3 `RUNNER~1` path fails four tests, CI probe); run `tests/Invoke-MuxAgentTests.ps1`, which excludes tag `windows` off Windows and exits 1 on any failure.

**Release (#5).** `.github/workflows/release.yml` on tags `v[0-9]+.[0-9]+.[0-9]+`, jobs chained by `needs`:
- `validate` (read-only) runs `scripts/release/Test-MuxAgentRelease.ps1`, which takes the tag, the tag SHA, the `origin/main` ancestry answer, the `ci.yml` push runs for that SHA, and the manifest version, and exits 1 naming the first failure: not on `main`, no successful CI run, or version mismatch.
- `package` (`needs: validate`, read-only): `git archive` of `.claude-plugin`, `skills`, `scripts`, `hooks` (when present), `README.md`, `LICENSE` into `mux-agents-<tag>.zip`, uploaded as an artifact.
- `publish` (`needs: [validate, package]`), the only job with `contents: write`: `gh release create <tag>` with the zip, `--generate-notes`, and `docs/releases/<tag>.md` prepended when present.

**Merge procedure** (no branch protection on this plan), in `CONTRIBUTING.md` and the PR template: read `gh pr view <n> --json headRefOid,statusCheckRollup`; every check on that head SHA must be `SUCCESS`, with `lint` and all three `test` jobs present; then `gh pr merge <n> --squash --match-head-commit <sha>`, so a newer push cannot be merged unchecked.

**Hygiene.** From SonicTerm: issue templates `bug.yml` and `feature.yml` (version, OS, multiplexer, client), a PR template (summary, type, closed issue, test plan, merge checklist), `dependabot.yml` for `github-actions` monthly.

## Component map

| component | job | interface | reuses or extends | likely change | test seam |
|---|---|---|---|---|---|
| `scripts/MuxAgents.ps1` | library, renamed | `MuxAgent*` functions | `RmuxAgents.ps1` | B ports it | fake providers |
| `tests/Invoke-MuxAgentTests.ps1` | one test entry for CI and people | `pwsh -File`; exit 0/1 | README test command | tags when B lands | CI log |
| `tests/MuxAgents.Foundation.Tests.ps1` | proves this sub-project on every OS | Pester | entry, library, git | B flips the Unix platform case | all runners |
| `scripts/release/Test-MuxAgentRelease.ps1` | release gate decision | parameters in, exit code and reason out | SonicTerm `validate-release-tag` | more gates | unit tests with given inputs |
| `ci.yml` / `release.yml` | checks; tag → release | `lint`, `test (<os>)`; tag push | SonicTerm workflows | B adds jobs | actionlint |

## Behavior and its tests

All in `MuxAgents.Foundation.Tests.ps1` unless noted; every one runs in the must-pass command.

- `pwsh -File scripts/mux-agent.ps1` with no verb prints JSON with `check` `usage` and a message naming `mux-agent`, exit 2 (Windows); on Linux and macOS it prints `check` `platform`, exit 2.
- With `MUX_AGENTS_STATE` unset and `RMUX_AGENTS_STATE` set, the state root is `%LOCALAPPDATA%\mux-agents\state`; with `MUX_AGENTS_STATE` set, that wins.
- A notice for a record with `parentAgentId` = a valid 12-hex id and `parentPane` `%7` lands in `mail/pane-7.jsonl` and not in `mail/<id>.jsonl`.
- `git grep -n -E 'rmux-agent|RmuxAgent|RMUX_AGENTS'` over tracked files outside `docs/specs` returns nothing. Tests that need an old name (this scan, the ignored variable) build it from fragments, e.g. `'R' + 'MUX_AGENTS_STATE'`, so the scan has no exceptions.
- `Test-MuxAgentRelease.ps1`: passes for an on-main SHA with a successful run and matching version; exits 1 with a distinct reason for off-main, no run, only failed runs, and a version mismatch (one test each).
- `release.yml` declares `publish` with `needs` containing `validate` and `package` (asserted by parsing the YAML text in a test).

## Must-pass

`pwsh -NoProfile -File tests/Invoke-MuxAgentTests.ps1` passes locally on Windows and in all three CI jobs; actionlint is clean; each PR is merged only through the merge procedure.

## Non-goals

Unix support (B); side bar (C); README rewrite, icon, launch (D); Pester 5; branch protection.

## Prior art

| practice | source | fit with our constraints | evidence |
|---|---|---|---|
| Read-only token; write only in `publish` | SonicTerm `release.yml` (read 2026-10-30) | Same | `permissions: contents: read` |
| Actions pinned by SHA; Dependabot monthly | SonicTerm `ci.yml`, `dependabot.yml` | Same | `actions/checkout@3d3c42e… # v7.0.1` |
| Tag on `main` with a successful CI push run | SonicTerm `validate-release-tag` | Same, as a tested script | `merge-base --is-ancestor`; runs query |
| Pinned tool verified by SHA-256 | SonicTerm `install-pinned-tool.sh` | Inline step | pinned digest |
| Pester 4 legacy `Should` on pwsh 7 | local Pester 4.10.1 run | Keeps 562 assertions | 179 passed / 0 failed |
