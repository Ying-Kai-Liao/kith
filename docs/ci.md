# CI

One workflow, `.github/workflows/ci.yml`. The single required check is the umbrella job `ci`: it passes when every job it needs passed or was skipped by a path filter, and fails on any failure or cancellation. Every new job must be added to its `needs` list.

## Jobs

| Job | Runner | What |
| --- | --- | --- |
| `changes` | linux | Path filters (below). |
| `host` | linux | `cargo fmt --check` and clippy on the whole host workspace. |
| `host-build` | linux | Builds all host test binaries once as a cargo-nextest archive (uploaded as an artifact). |
| `host-test` | linux | Runs the archive's unit tests. Config: `serve/host/.config/nextest.toml`, profile `ci`. |
| `host-e2e` (1..3) | linux | The archive's `tests/*.rs` integration tests, in three `--partition` shards. Heavy tier. |
| `host-crates` | linux | Tests and doctests of guest, tts, wire and memory (nextest does not run doctests). |
| `guest-linux` (heavy) | linux x86_64 and arm64 | Static kith-guest build, installer and smoke test. |
| `cloud`, `relay`, `licenses`, `kernel`, `thread-writers` | linux | As named. |
| `desktop-js`, `desktop-relay-e2e` (heavy) | linux | Desktop typecheck and unit tests; the relay e2e against a real kith-serve. |
| `ios`, `desktop` (heavy) | macos-26 | Only what needs macOS: the iOS build and the unsigned Mac app build. |

No paid, larger or self-hosted runners. Rust caches come from Swatinem/rust-cache and are saved only on main (PR and batch refs cannot share caches and would evict main's). npm and SwiftPM caches are kept per job.

## Tiers

- Cheap tier, every pull request: fmt, clippy, host unit tests, kernel, cloud and relay (typecheck, vitest), licenses, thread-writers, desktop typecheck and tests. All path-filtered.
- Heavy tier: the macOS builds (`ios`, `desktop`), every e2e (`host-e2e`, `desktop-relay-e2e`, cloud's bot guests e2e step) and `guest-linux`. They run on the reviewer's batch PR (head ref `flow/batch-<sha>`, heavy on its own `ci` run), push to main and any other workflow_dispatch, never skipped there.
- A pull request gets the heavy tier too when it carries the `ci:full` label (add it with `gh pr edit <n> --add-label ci:full`). A draft pull request gets the cheap tier only, even with the label: `ci-label.yml` doesn't dispatch the heavy run for a draft. Mark it ready to run the heavy tier.

Jobs skipped by the tier count as passed in `ci`, so a green PR means "cheap tier green"; the reviewer's batch PR runs the rest before anything lands on main.

## Path filters

Filtering happens in `changes`, not in `on.paths`, so `ci` always reports. A docs-only change runs no heavy job. `host_core` (everything the host binary builds from) is narrower than `host`: a change only under `serve/host/crates/guest` or `crates/tts` skips `host-build` and `host-test`, because nothing in the host depends on them; `host-crates` still runs.

The filters apply on pull requests, on pushes to main and on dispatch.

## Merging

This repo uses merge commits. The maintainers' flow reviewer merges PRs in batches: it merges the batch locally on top of the latest main, pushes it to a `flow/batch-<sha>` branch and opens a batch PR there, whose own `ci` run is heavy (a `flow/batch-` head ref turns the heavy tier on, no label or dispatch needed); only when that is green does it push to main. There are no intermediate or integration branches. To run the heavy tier on a single PR, add `ci:full`.

Never `gh pr merge` directly: that skips the heavy tier.

Never rebase or merge main into a branch just to be up to date. Do that only to resolve a real conflict.

## Concurrency

A PR gets one group per ref, so a re-push cancels its older run. Pushes to main are keyed by sha and run id and are never cancelled. A batch PR is a PR like any other, so a batch branch force-pushed to a new sha cancels the older batch run. Label events never start `ci.yml`, so a label creates no run and no skipped `ci` check. Adding `ci:full` runs the small `ci-label.yml`, which dispatches `ci.yml` on the PR branch (a dispatch run is heavy, ignores draft state, and its `ci` lands on the head sha); a cancelled run skips `ci` instead of reporting a failure.

## Windows clippy on a Mac

The `windows` job used to clippy one crate per step, so the first failing crate hid the rest and each fix cost a CI round (about 40 minutes). `scripts/ci/windows-clippy.sh` now holds the whole Windows clippy list (the all-targets crates in one `cargo clippy -p ... --all-targets` call, `kith-serve --bins` in a second, both `--locked -D warnings`), runs both even when the first fails, and names every failed group. The job runs it as its first cargo step; the later steps only test and build. The script is the source of truth for the list: a new crate that CI builds on Windows is added there.

**When:** any change to a Windows-built crate (kith-holder, kith-winsys, kith-core, kith-api, kith-kernel, kith-stub, kith-attach, kith-serve) or to a `cfg(windows)` / `cfg(not(unix))` path. Run it once before pushing; it replaces a CI round.

```sh
scripts/ci/windows-clippy.sh --target x86_64-pc-windows-msvc
```

Run it under `mcp__flow__test_slot`, at background priority: `nice taskpolicy -b scripts/ci/windows-clippy.sh --target x86_64-pc-windows-msvc`.

It cross-checks through `cargo xwin clippy` (C build scripts such as kith-core's bundled SQLite need the Windows headers). One-time setup, which the script explains if a piece is missing:

- `brew install llvm` (clang-cl and llvm-lib for the C build scripts)
- `cargo install --locked cargo-xwin`
- `rustup target add x86_64-pc-windows-msvc` in `serve/` (the toolchain pinned by `serve/rust-toolchain.toml`)
- the script sets `XWIN_ACCEPT_LICENSE=1` (accepts the Microsoft CRT/SDK license; approved) and `CC=/usr/bin/cc` for host build scripts.

Output goes to its own target dir, `serve/host/target-xwin` (override with `WINCLIPPY_TARGET_DIR`), so it never invalidates the Mac build cache. A second run reuses it.

**Cost** (measured on the flow Mac, `nice taskpolicy -b`, at origin/main c65afbf05; clean, every Windows-clippied crate checks):

- First-time setup: `cargo install --locked cargo-xwin` about 5 min; `brew install llvm` (clang-cl and llvm-lib; about 2 min, roughly 1.5 GB); the xwin cache downloads the Windows CRT/SDK, about 1.1 GB on disk (`~/Library/Caches/cargo-xwin`).
- Cold run (empty target dir): about 4.5 min of wall time after the setup (a 12 min first attempt also compiled the registry crates, which are shared with other worktrees' downloads).
- Warm run (nothing changed): about 2.5 s. After an edit, only the changed crates and what depends on them recheck.
- Disk: `serve/host/target-xwin` about 1.3 GB per checkout, plus the shared xwin cache above.

Delete `serve/host/target-xwin` to reclaim the space; nothing else depends on it.

Fallback when you cannot run it: the `rust-check` workflow_dispatch run (mode=check, os=windows) is a CI round on a runner.

### Local clippy uses the pinned toolchain

A Mac without a rustup proxy on PATH ignores `serve/rust-toolchain.toml` and runs whatever `cargo` it finds (stable 1.93), which shows lints CI never sees. `windows-clippy.sh --target ...` reads the channel from the toolchain file and puts that toolchain first on PATH, and fails clearly if the toolchain or its windows-msvc rust-std is missing. For the ordinary (Linux/Mac) clippy, do the same by hand:

```sh
export PATH="$HOME/.rustup/toolchains/1.99.0-aarch64-apple-darwin/bin:$PATH"   # the channel in serve/rust-toolchain.toml
```
