# Contributing

One pull request per issue, on its own branch.

Commits are authored and committed as `D0n9X1n <D0n9x1n@outlook.com>`. A pull request lands by fast-forwarding `main` to its tested head, never through a GitHub merge, which would re-author it.

## Tests

`pwsh -NoProfile -File tests/Invoke-MuxAgentTests.ps1` (Pester 4.10.1) must pass on Windows. CI runs it on Linux, macOS and Windows.

## Merge procedure

1. `gh pr view <n> --json headRefOid,statusCheckRollup`
2. Every check on that `headRefOid` is `SUCCESS`, and `lint`, `test (ubuntu-latest)`, `test (macos-latest)` and `test (windows-latest)` are all present.
3. `git fetch origin <sha>`, then `git push origin <sha>:main`, with `<sha>` the `headRefOid` from step 1. The push is not forced, so it lands only when `<sha>` builds on the current `main`; GitHub then marks the pull request merged.

## Releases

Bump `version` in `.claude-plugin/plugin.json` on main, optionally add the release page `wiki/Release-v<version>.md` (and `wiki/Release-v<version>-zh-CN.md`), then push tag `v<version>` on a main commit whose CI push run is green.
