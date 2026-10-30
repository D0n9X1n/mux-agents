## Summary

## Type
- [ ] Fix  - [ ] Feature  - [ ] Chore or docs

## Closes
Closes #

## Test plan
- [ ] `pwsh -NoProfile -File tests/Invoke-MuxAgentTests.ps1` passes locally on Windows

## Merge checklist
- [ ] `gh pr view <n> --json headRefOid,statusCheckRollup`: every check on headRefOid is SUCCESS; `lint`, `test (ubuntu-latest)`, `test (macos-latest)` and `test (windows-latest)` are present
- [ ] `gh pr merge <n> --squash --match-head-commit <sha>`
