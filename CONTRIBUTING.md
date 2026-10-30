# Contributing

One pull request per issue, on its own branch, squash-merged.

## Tests

`pwsh -NoProfile -File tests/Invoke-MuxAgentTests.ps1` (Pester 4.10.1) must pass on Windows. CI runs it on Linux, macOS and Windows.

## Merge procedure

1. `gh pr view <n> --json headRefOid,statusCheckRollup`
2. Every check on that `headRefOid` is `SUCCESS`, and `lint`, `test (ubuntu-latest)`, `test (macos-latest)` and `test (windows-latest)` are all present.
3. `gh pr merge <n> --squash --match-head-commit <sha>`, with `<sha>` the `headRefOid` from step 1.

## Releases

Bump `version` in `.claude-plugin/plugin.json` on main, optionally add `docs/releases/v<version>.md`, then push tag `v<version>` on a main commit whose CI push run is green.
