# CLAUDE.md

Rules for agents working in this repository. [CONTRIBUTING.md](CONTRIBUTING.md) has the layout and the must-pass checks.

## Comments never cite an issue or a pull request

Never refer to an issue, a pull request, a CI run or a review thread in code, tests, CI files, fixtures or the wiki: no `#` followed by a number, no `PR` number, no issue or run URL. The code may move to another hosting platform, and those numbers and links then point nowhere.

Write what a reader needs instead:

- **Why** the code exists or behaves this way: the problem it solves or the failure it prevents.
- **How** it works, when that is not obvious from the code.
- **How to do it**, when a caller or a later change must follow a rule.

For example, not a comment that only names the issue it fixed, but `// A 0.2.x member still running after the upgrade is listed, so a person sees what holds its team slot.`

Commit messages and pull request descriptions may name issues and pull requests; the repository's files may not. `tests/repo.rs` (`no_issue_or_pull_request_numbers`) fails on any `#` followed by digits that reads as such a number.
