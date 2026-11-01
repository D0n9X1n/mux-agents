## Summary

## Type
- [ ] Fix  - [ ] Feature  - [ ] Chore or docs

## Closes
Closes #

## Test plan
- [ ] `cargo fmt --check`, `cargo clippy --all-targets --features test-panic --locked -- -D warnings` and `cargo test --features test-panic --locked` pass locally
- [ ] The other Must-pass commands in CONTRIBUTING.md that this change touches pass locally

## Merge checklist
- [ ] `gh pr view <n> --json headRefOid,statusCheckRollup`: every check on headRefOid is SUCCESS; `lint`, `test (ubuntu-latest)`, `test (macos-latest)`, `test (windows-latest)`, `integration (ubuntu-latest)` and `integration (macos-latest)` are present
- [ ] `git fetch origin <sha>`, then `git push origin <sha>:main`, with `<sha>` the checked headRefOid; not forced, so it lands only on the current `main`
