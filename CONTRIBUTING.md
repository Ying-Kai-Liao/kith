# Contributing

## CI and merging

Pull requests run a fast tier of checks (lint, unit tests, typecheck). The slow ones (macOS builds, e2e tests) run on the maintainers' batch and on push to main; add the `ci:full` label to run them on the PR itself.

Maintainers' flow reviewer merges PRs in batches with merge commits: it builds the batch locally on top of the latest main and dispatches the heavy `ci.yml` on the batch branch before pushing to main. There are no intermediate or integration branches.

Never `gh pr merge` directly: that skips the heavy tier. Don't rebase or merge main into a branch just to be up to date; only to resolve a real conflict. Details: [docs/ci.md](docs/ci.md).

## License

Contributions are licensed under Apache-2.0 (inbound = Apache-2.0, no CLA).
