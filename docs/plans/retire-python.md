# Retiring the Python kernel

Status: plan, 2026-11-17. Docs and inventory only: this PR changes no code, test or script. The retirement itself runs
later as the manager `retire-python`, after the `core-transcripts`, `core-schedules` and `rebuild-followups` work has
merged. This file is that manager's map.

Why now: Kai, 2026-11-17: "I think after those, we can retire the python one." The Rust core already answers the verbs
listed in `kernel/lib/core-switched.txt` and owns the state of the `accounts` and `authority` modules on Kai's Mac
(per main's cutover note of 2026-11-16, to be re-read with `kith migrate status` before the first step). What is left is
a long tail of Python that still runs on every launch, on every hook and in the host's subprocess calls.

Read with: `docs/plans/one-bot-model.md` (M0-M5 and its dated log), `docs/plans/core-design.md` (section 3 the router,
section 11 the switch evidence, Q2 "when is the binary mandatory"), `docs/plans/runtime-design.md`,
`core/runtime/src/cutover/mod.rs` (the framework) and `docs/codebase-map.md`.

## 0. How the two sides fit together today

Three mechanisms already exist. The plan reuses them and invents as little as possible.

| Mechanism | Where | What it does | What it cannot do for retirement |
|---|---|---|---|
| Verb switch | `kernel/lib/core-switched.txt` read by `kernel_core_exec` in `kernel/lib/kith-core.sh` | A verb line (`verb<TAB>module[<TAB>cutover module]`) sends `kith VERB` to `kith kernel VERB`, when this Mac has a `kith` program whose `kith kernel version` lists the module. Otherwise it returns 1 and the Python answers, silently. Removing the line is the rollback. | The fallback to Python is silent, so a Mac with an old or missing program never notices. Rollback needs the Python still to be there. |
| Cutover module | `core/runtime/src/cutover/` (`kith migrate step\|status\|check\|rollback MODULE`, `kith rollback MODULE`); registry in `cutover/mod.rs` `registry()`: `accounts`, `transcripts`, `claims`, `schedules`, `authority` | Moves a Python-owned store (state.json, claim files, sched state, auth.json) into core.db, checks the answers match, writes a switch file `<machine>/core/cutover/MODULE.switch`, replaces the source files with tombstones. `rollback` writes the files back (`export_back`, `restore_secrets`). | It is a per-Mac, deliberate act ("never automatic"). `kith migrate` and `kith rollback MODULE` are themselves dispatched through Python today (section 1.4). |
| Tombstone | `core/runtime/src/cutover/tombstone.rs` writes, `kernel/lib/kith_tombstone.py` reads; `kith_cutover_refuse` in `kith-core.sh` | A Python reader that meets a tombstone refuses (`Tombstoned`, exit 3 or 4). `KITH_CORE=python` is refused for a switched module. | It is per file or per module. There is no per-verb "this script moved" stub for a script that has been deleted. |

Two more facts shape the plan.

- The Python files travel in every package. `scripts/stage-kernel.sh` copies the whole `kernel/` tree (minus `tests/`)
  into the release tarball, the Homebrew formula (`depends_on "python@3"`) and the Mac app (`apps/desktop/tools/native-host.mjs`
  calls the same script). A release installs as `<prefix>/cli/<version>/`, `cli/current` points at it, and the last
  `KITH_KEEP_VERSIONS` (default 3) older versions stay (`scripts/install.sh`). **This is the rollback and compatibility
  mechanism the plan leans on: deletion happens in a new release folder, never in place.**
- The host and the apps do not run Python themselves. The Rust host (`serve/host`) spawns kernel scripts by path
  (`kernel/bin/kith-auth`, `kith-wake`, `kith-door`, ...) or through the `kith` entry (`serve/host/src/kernel.rs`,
  `Route::Kernel` hands every verb it does not own to `kernel/bin/kith`). The desktop app (`apps/desktop`) and iOS
  (`apps/ios`) call the host's API only; grep finds no direct script invocation in either. So "old app builds still calling
  Python verbs" means old **host binaries** (bundled in an old desktop app, or a standalone install), not the apps' UI code.

## 1. Inventory

Counts of "callers" come from `grep -rl` over `serve/`, `core/`, `apps/`, `scripts/`, `packaging/`, `.github/` and
`kernel/` (tests excluded) on `origin/rust-rebuild` at `48ed39f13`. The first column is the file; line counts are
`wc -l`. "Rust" means code exists; "routed" means a caller actually uses it today.

### 1.1 kernel/bin: bash

| Piece | Lines | Role | Wrapper only? | Python inside |
|---|---|---|---|---|
| `kith` | 1561 | The launcher and the verb dispatcher: pin the package, assemble the run, walk backup accounts, `--held`, `--launch-json`. `exec python3 kith-sessions run` is its last line (1561); `exec python3 adapters/harness.py` the one-shot path (1375-1377). | No | 56 `python3` mentions: `kith_paths.py --sh`, `--bin`, `--machine`, `kith_accounts.py backups-json`, `kith_settings.py set`, `kith_paths.py`/`kith_engines.py` fallbacks, inline `python3 -I -c` JSON. `kith-core.sh` itself calls `python3 kith_paths.py --bin` and `--machine` (the router needs Python to find the Rust program). |
| `hq` | symlink to `kith` | Old name. `serve/host/src/kernel.rs` also routes `hq` to the kernel. | Yes (symlink) | none |
| `kith-new` | 147 | Scaffolds `agents/<name>/`; `hq-new` is a symlink. | No | 4 |
| `kith-send` | 152 | Delivers a result (Discord, outbox, Telegram); `hq-send` symlink. | No | 6, JSON building |
| `kith-serve` | 244 | `kith serve start\|stop\|restart\|status\|relay\|cloud`: builds the host, installs `dev.kith.serve`; `hq-serve` symlink. | No | 2 |
| `kith-call` | 72 | Calls another agent (`kith call`). | No | 1 (`python3 -I -` body) |
| `lib/kith-core.sh` | 292 | The router (`kith_core_exec`, `kith_cutover_refuse`, `kith_cost`, held-terminal helpers). | No | `python3 -I -c` JSON helpers, `kith_paths.py` |
| `lib/kith-env.sh`, `lib/guest-env.sh` | 23, 7 | Env sourcing. | Yes | none |

No bash script is only a wrapper except the `hq*` symlinks. Every bash script carries inline Python for JSON or paths,
so "no Python" needs Rust verbs for those fragments too, not only for the `.py` files.

### 1.2 kernel/bin: Python (shebang `python3` unless noted)

The `hq-auth`, `hq-cost`, `hq-doctor`, `hq-pkg`, `hq-state`, `hq-who` entries are symlinks to the `kith-*` file of the
same name; they carry no code of their own. Callers below are non-test files.

| Script | Lines | Called by | Rust | What is missing |
|---|---|---|---|---|
| `kith-auth` (`hq-auth`) | 3724 | Host: `access.rs`, `api/{people,roles,botguests,authority,devices}.rs`, `cards/*`, `channels/{hq,core,manager}.rs`, `chat/{group,room_*}.rs`, `agent/hq.rs`, found beside the kith binary (`channels/hq.rs` `auth_bin`). Core: `cost/views.rs`, `people/system.rs`, `launch/guard.rs`. `bin/kith`, `kith-permit`, `kith-pkg`, `kith-doctor`, 9 `lib/` modules. Desktop: only as text in comments and tests (16 files), no call. | `core/runtime/src/authority/` (revoke, readers, core.db tables), `core/kernel/src/{secrets,people}/`, `kith kernel auth resolve\|may`. On a switched Mac `kith-auth` already routes most shapes to the core (L1 lift routes, #974) and exits 3 for the rest. | A Rust CLI for every shape (`kernel auth` has only `resolve` and `may`). The shapes that exit 3 on purpose: room secrets (`once`, `exec --secret`, q236), `job AGENT ID JOB` ticks, `mcp-config` with more than one argument. The people-admin writes are app-only on a switched Mac (documented). The unswitched-Mac code path (auth.json plus Keychain) is the importer's job only. |
| `kith-permit` | 772 | Every run: the `kithpermit` MCP server (`core/kernel/src/launch/mcp.rs:14`, `assemble.rs:1425`), the auto-mode `PermissionDenied` and `PreToolUse` hooks (`launch/settings.rs:977-984`), `apply` (`assemble.rs:1392`). Host: `cards/*` talk about it. | `core/kernel/src/permit/` (`decide`, `helper`, `door`, `hooks`, `host`, `issue`, `access`, `apply`, `mcp`), `kith kernel permit`. Goldens `permit.json`, `permit-mcp.json`. | Routing: nothing points the hooks or the MCP command at `kith kernel permit`. `ask_helper` shells to `bin/kith-ask` (`permit/helper.rs:76`). |
| `kith-sessions` | 1105 | Every run: SessionStart hook `record` (`settings.rs:776`) and the launcher's last exec `run`. Host: `terminal/`, `remote/account.rs`; `kith-wake`, `kith-lifecycle`. | `core/kernel/src/threads/` (`kith kernel threads`), `core/runtime/src/conversations/` (claims), module `claims`. In Writer mode the Python takers already call the core (#994). | `kith kernel threads` is not routed. `run` (the take-over note and exec of claude) has no Rust twin. The claims module is stepped on no Mac (#1010 lifted the refusal). |
| `kith-wake` | 762 | Host: `serve/host/src/wake.rs` (one process per call: `enqueue`, `take`, `done`, `back`, `withdraw`), `cards/resume.rs`. `kith-sessions`. | `core/runtime/src/wake/` (queue, model, turns), import `import/wake.rs`. Golden `wake.json`. | The host still spawns the script; a core-side caller in `wake.rs` is the switch. Frozen but not routed. |
| `kith-lifecycle` | 391 | `kith-sessions`; host `agent/bot/lifecycle.rs`; `host/crates/memory/src/compaction.rs`. | none (the compact claim moved to `kith-stub`, #990, still Python). | Everything. `lifecycle.json` golden exists. |
| `kith-sched` | 749 | Host: `agent/hq.rs`, `api/kith_info.rs`, `rich_card.rs`, `run_outbox.rs`. `dev.kith.sched` launchd job (not installed on Kai's Mac). Desktop and iOS: via `kithInfo` schedules, no direct call. | `core/runtime/src/schedules/` (tick, fire, run now, webhook), `core/kernel/src/schedule/` (Trigger, cron), module `schedules` (rolled back on Kai's Mac 2026-11-16). | The cutover cannot be stepped until the core runs fired turns (`schedules_gate.rs`: four facts; #1011, #1015 landed the host runner; `core-schedules` is finishing it). CLI verbs `kith schedule ls\|add\|rm\|pause\|resume\|run\|log --json` as Rust: to be confirmed by `core-schedules`. |
| `kith-cost` (`hq-cost`) | 650 | `SessionStart` hook `tag` (`settings.rs:774`), `launch/assemble.rs:733`, `kith_cost()` in `kith-core.sh`, launchd `dev.kith.cost` (by hand). Host: 4 files. | `core/kernel/src/cost/` (`limit`, `spent`, `usage --json`, `agent --json`, `members`, `bots --json`, `sync`); `cost` is in `core-switched.txt` for exactly those lines. | `tag` and `record` (the ledger's only writer, core-design Q7), the text reports, bare `kith cost`. |
| `kith-account` / `lib/kith_accounts.py` | 731 / 1122 | `bin/kith`, `kith-pkg`, `kith-scratch`, `kith-doctor`. Host: `remote/account.rs`. | `core/kernel/src/accounts/` (list, pick, change, state, mirror), module `accounts` (switched on Kai's Mac). | `backups-json` / `accounts-json` are in the chain verbs (`kith kernel chain`); the login-command verbs of `kith account add\|remove` to be confirmed. |
| `kith-pkg` (`hq-pkg`) | 1680 | `bin/kith` (`resolve`), `launch/assemble.rs:855`, `serve/host/src/launch_cmd.rs:67`, host `api/scratch.rs`, `install.sh`, `kith-doctor`. | `core/kernel/src/pkg/` (`resolve`, fetch, store, git, problems), `kith kernel pkg resolve`; `pkg` switched in `core-switched.txt`. | Every other verb: `install`, `update`, `share`, `search`, `restore`, `gc`, `pin`/`unpin`/`rollback`, `history`, `push`, `commit`, `deps`, `check --share`, `list`. The `--json` forms are frozen by `kith-pkg-json-check`. |
| `project-id` | 248 | `bin/kith` (`project_id()`), host `runauth.rs:966` (guest runs reach it over the run socket), `kith-project`, `kith-pkg`. | `core/kernel/src/project/`, `kith kernel project-id`; `project-id` switched. | The guest branch (a run with `KITH_RUN_TOKEN` asks the host through `kith_runsocket.py`) is refused by the Rust verb and stays Python. |
| `kith-room-ledger` | 65 | Pre/PostToolUse hook of every launch (`settings.rs:541`). | `core/kernel/src/room_ledger/`, `kith kernel room-ledger pre\|post`; golden `room-ledger.json`. | Routing: the launch still writes the script path as the hook. |
| `kith-people` | 195 | `kith people`, `kith invite`. Host: `api/people.rs`. | `core/kernel/src/people/`, `kith kernel people` (reads through `kith-auth people --json`). | Switch: waits for a core read verb; `people` is not in `core-switched.txt`. |
| `kith-memory` | 226 | Agents (allowed without a prompt: `settings.rs:714`), host `runauth.rs` (guest `search` over the run socket), `mcp.rs`. | The host answers `check`, `log`, `diff`, `undo`, `rm`, `forget`, `tidy`, `hook`, `summary` (`kernel.rs` `MEMORY_VERBS`). | `ls` and `search` (and the guest branch through `kith_runsocket.py`). |
| `kith-recall` | 167 | Host `hqmem/tidy.rs:984`, agents (guest branch asks the host: `ask_host`). Uses `lib/kith_recall.py` (448). | Host side only: `serve/host/src/runauth.rs`, `core/runtime/src/runsock.rs`. | The client. |
| `kith-allow` + `lib/kith_allowed.py`, `lib/kith_guard.py` | 132 + 279 + 728 | Host `cards/autoallow.rs` (`--derive`, `--add-json`), `launch/guard.rs:205`, `bin/kith` settings fill, `kith-pkg check`, `kith-permit`, `kith-auth`, `lib/kith_jobs.py`. | The must-ask judgement is mirrored in `core/kernel/src/launch/guard.rs` and `serve/host/src/cards/resource.rs`. | `--derive` and `--add-json` (the host's "always allow" path) have no Rust twin; `kith_allowed.py` decides what is remembered. |
| `kith-door` + `lib/kith_door.py` | 19 + 661 | Host `doors.rs` (`sweep` at start, on a cut-off, every minute), `kith-permit`, `lib/kith_jobs.py`. | `core/runtime/src/doors/` (row state, sweep) and host `doors.rs`, not switched. | The process side (start the box, supervise); the Python state is the live one. |
| `kith-ask` | 1142 | Hooks and the permit helper (`permit/helper.rs:76`), host `api/cards.rs`, `cards/mod.rs`, `remote/account.rs`; 8 serve files. | State only: `core/runtime/src/asks/`, `import/asks.rs`, `delegation` rules in `core/kernel/src/delegation.rs`. | The command itself. Golden `asks.json`, `delegation.json`. |
| `kith-acp` | 255 | Host `agent/acp.rs`; `adapters/acp/{claude-via-kith,shim/claude}` (a Python shim). | The ACP client crate `core/acp` is a different thing (the host's engine transport). | The ACP server for outside clients. |
| `kith-info` | 402 | Host `agent/hq.rs` (`hq::info`, cached), `api/kith_info.rs`; the apps read it through `kithInfo`. | none | Everything; the `info --json` shape is the apps' contract (`serve/docs/bot-info.md`). |
| `kith-doctor` (`hq-doctor`) | 594 | You. Reads launchd (`kith-uv` stuck check, `kith-doctor:460`), `lib/kith_migrate.py`. | none | Everything. |
| `kith-init` | 526 | `install.sh`, you. | none | Everything (checks git/claude/gh/uv, makes the state repos, offers backups, starters). |
| `kith-state` (`hq-state`) | 288 | You; launchd `dev.kith.state` (03:00 backup, `ProgramArguments` = `kith-uv run --script kith-state backup --yes`), `lib/kith_launchd.py`. | none | Everything. |
| `kith-self-update` | 223 | `kith update` (no agent), `kith-doctor --check`, `scripts/install.sh`. | none (the host has its own updater, `serve/host/src/update/`, for the host binary only). | Everything; it drives `install.sh`, `brew upgrade`, `git pull --ff-only`. |
| `kith-remote` | 411 | You, `kith-ask`. | none | Everything. |
| `kith-migrate` + `lib/kith_migrate.py` | 26 + 900 | `kith migrate`, `kith rollback MODULE` (`bin/kith:151-153`), `kith-doctor`. | The cutover itself is Rust (`kith kernel cutover`); `kith_migrate.py:869-887` only forwards `step\|status\|check\|rollback\|is-module` to it. The `hq` to `kith` rename steps are separate work, not this package. | The forward moves to the dispatcher; the rename steps stay until the rename is declared done. |
| `kith-skills`, `kith-scratch`, `kith-project`, `kith-group`, `kith-send-to`, `kith-connector`, `kith-keys`, `kith-cards`, `kith-who`, `kith-bug`, `kith-chain`, `kith-helper-allow` | 311, 181, 242, 407, 186, 156, 94, 89, 154, 197, 240, 37 | Mostly the user or the host by name: `kith-send-to` (`chat/group.rs:769,1462`), `kith-cards` (`cards/history.rs`), `kith-group` (desktop), `kith-connector` (host `mcp.rs`), `kith-scratch` (`api/scratch.rs`). `kith-chain` has no caller outside its own check and CI. | `kith kernel chain` (backup chain verbs) covers part of `kith-chain`'s concerns only. | Each is a port or a drop (section 8). |
| `kith-serve-launch` | 60 | `dev.kith.serve` when a launcher is set: `["~/.kith/bin/kith-uv","run","--quiet","--script","<repo>/kernel/bin/kith-serve-launch", ...]` (`serve/host/README.md`). `#!/usr/bin/env -S uv run --quiet --script`, the only `uv` shebang. | none | A launcher that passes signals and SIGKILLs after 20 s; see section 5. |

### 1.3 kernel/lib and kernel/adapters

| File | Lines | Imported by | Rust | Notes |
|---|---|---|---|---|
| `kith_paths.py` | 341 | Every script and `bin/kith` (`--sh`, `--bin`, `--machine`); `kith-core.sh` | `core/kernel/src/paths/`, `kith kernel paths`, `paths` switched | Last to go: the router uses it to find the Rust program. The installer's `machine_dir` logic is a second copy. |
| `kith_settings.py` | 629 | `bin/kith`, `kith-cost`, `kith-pkg`, `kith-doctor`, `kith-ask`, `kith_accounts.py` | `core/kernel/src/settings/`, `kith kernel settings`, `settings` switched (the `set` verbs) | Reads still Python in the scripts that import it. |
| `kith_engines.py` | 32 | `bin/kith` | `core/kernel/src/engines.rs`, `engines` switched | Python only prints "needs the kith program": already a stub. |
| `kith_accounts.py` | 1122 | see `kith-account` | `accounts` | see above |
| `kith_claims.py` | 118 | `kith-sessions`, `kith-wake`, `bin/kith` | module `claims` | Reads the claims marker; goes with `claims`. |
| `kith_procstart.py` | 226 | `kith-sessions`, `kith-wake`, `kith_lifecycle.py` | `core/procstart`, `core/kernel/src/procstart.rs` | Check `kith-procstart-check` asserts both write the same string. |
| `kith_lifecycle.py` | 350 | `kith-lifecycle`, `kith-sessions` | none | |
| `kith_guard.py`, `kith_allowed.py`, `kith_jobs.py` | 728, 279, 240 | `kith-auth`, `kith-pkg`, `kith-permit`, `kith-allow`, `bin/kith`, `kith_door.py` | partial, see `kith-allow` | |
| `kith_door.py` | 661 | `kith-door`, `kith-permit`, `kith_jobs.py` | `core/runtime/src/doors/` (state) | |
| `kith_runsocket.py`, `kith_recall.py` | 46, 448 | `kith-memory`, `project-id`, `kith-recall` | host `runauth.rs`, `core/runtime/src/runsock.rs` (server side) | |
| `kith_readfence.py` | 65 | `bin/kith`; the launch's fallback hook (`settings.rs:1404-1410`) | `core/kernel/src/readfence.rs`, `kith kernel readfence`; the launch uses it when it knows a kith binary | Python is only the fallback. Leaf. |
| `kith_facets.py`, `kith_trajectory.py`, `kith_redact.py`, `kith_bug.py`, `kith_chain.py`, `kith_host.py`, `kith_runenv.py`, `kith_tombstone.py` | 530, 424, 90, 220, 124, 41, 49, 78 | scripts above | `kith_facets` partly in `core/kernel/src/launch/facets.rs` | `kith_tombstone.py` has to outlive the last Python reader of a tombstoned file. |
| `kith_launchd.py` | 70 | `kith-sched`, `kith-state` (install and reload jobs) | `serve/host/src/service/unix.rs` mirrors it | |
| `hq_paths.py`, `hq_settings.py` | 16, 9 | nothing in `kernel/` imports them | none | Old-name shims; delete first. |
| `adapters/harness.py` | n/a | `bin/kith:1375-1377` (one-shot path) | `kith kernel run-once` (ACP engines; `serve/host/src/runonce.rs`) | Non-ACP engines still use Python. |
| `adapters/claude-code/hooks/register.tsx` | n/a | the Claude Code plugin | n/a | A TSX module that shells to `kith_facets`, `kith_paths`, `kith_settings` (grep of the imports); moves with them. |
| `adapters/acp/shim/claude` | n/a | `kith-acp` | none | `#!/usr/bin/env python3`. |

### 1.4 What every run still runs in Python

A launch goes through the Rust `launch` when the caller asked for `--launch-json` (the host does) and `launch` is
switched. Even that run starts Python at these points, all written by Rust into the run's settings:

- SessionStart hooks: `kith-cost tag` (10 s) and `kith-sessions record` (5 s), `core/kernel/src/launch/settings.rs:774-776`.
- PreToolUse and PostToolUse: `kith-room-ledger pre|post` (`settings.rs:541`); the read fence for guests is Rust when the
  launch knows a binary, else `python3 -I kith_readfence.py`.
- Auto mode: `kith-permit denied` (660 s timeout) and `kith-permit once` (`settings.rs:977-984`).
- The MCP server `kithpermit`: command `bin/kith-permit` (`launch/mcp.rs:14`).
- The launch's own calls: `kith-pkg resolve` (`assemble.rs:855`, unless the Rust resolver answers), `kith-cost`
  (`assemble.rs:733`), `kith-permit apply` (`assemble.rs:1392`).
- The interactive terminal run (not `--launch-json`): the whole bash launcher, the backup-account walk, `--held` helpers in
  `kith-core.sh` (`python3 -I` JSON), and the last exec, `kith-sessions run`.
- The host's subprocesses by path: `kith-auth` (the busiest), `kith-wake`, `kith-door`, `kith-send-to`, `kith-allow`,
  `kith-cards`, `kith-recall`, `kith-sched` (info), `kith-pkg resolve` (`launch_cmd.rs:67`), `kith-lifecycle`, `kith-memory`
  (guest search), `project-id` (guest), and `kith <agent> --info-json` through the entry.

Hooks are written fresh into each run's settings, so a changed hook command reaches the next launch. A run that is
already going keeps the command line it started with until it ends. A long conversation resumed later is a new launch.
This is the in-flight exposure for any step that moves a hook (section 2.1).

### 1.5 Packaging, installers, launchd

| Piece | Path | Python or uv in it | Retirement effect |
|---|---|---|---|
| Release tarball | `scripts/package-release.sh` calls `scripts/stage-kernel.sh` | Copies all of `kernel/` (Python included) into `share/kith/kernel`. | The deletion PRs make the tarball smaller; no packaging change needed until the last step. |
| Mac app | `apps/desktop/tools/native-host.mjs` calls `stage-kernel.sh`; `apps/desktop/src/main/host-controller.ts` `ensureCli` installs the bundled CLI (`Resources/kith-install.sh`) when the installed one is below `declared.cli` (`apps/desktop/package.json` `kith.cli`, currently `0.1.0`) | Same tree. | The app is how a Mac gets a new CLI (section 3). The check is `MIN_CLI` in `serve/host/src/kernel.rs:172` and `cli_status`. |
| Homebrew | `packaging/homebrew/kith.rb` | `depends_on "python@3"` | Drop at the last step. A clean Mac's `python3` is the Xcode tools stub (a popup); dropping it is a real gain. |
| Installer | `scripts/install.sh`, `install.sh` | `python3` for the atomic swap and drain (`quiesce.py`, line 146, 330-354), `kith_paths.py` for `auth.json` locks | The installer is the gate for a Python-free release; it must not depend on Python to install one. |
| Self-update | `kernel/bin/kith-self-update` | Python; drives `install.sh`, brew, git | Port or fold into `install.sh` plus the host updater. |
| Voice, screens, guest | `scripts/voice-samples/*.py`, `apps/ios/tools/make-kith-glyph-fonts.py`, `apps/tests/notification_response.py` | Python build tools | Not the kernel; out of scope. They stay. |
| Host test fixtures | `serve/host/tests/fixtures/*.py`, `serve/host/tests/fake_agent.py` | Python fake agents | Test-only; out of scope. CI keeps a Python for them. |
| `kith-uv` | `~/.kith/bin/kith-uv`, found by `kith_paths.launcher()`; `docs/moving-from-hq.md` | A copy of `uv` that holds Full Disk Access (a repo on an external disk) | Section 5. |
| launchd jobs | written by `kith-state` (`dev.kith.state`, 03:00), `kith-sched --install` (`dev.kith.sched`, every 60 s), `kith serve start` and the host's `service/unix.rs` (`dev.kith.serve`); by hand: `dev.kith.cost` (6 h), `dev.kith.monitor` and `dev.kith.house` (agents' own) | `ProgramArguments` is `[kith-uv, run, --quiet, --script, <kernel>/bin/<script>, args]` or the bare script when no launcher exists (`kith-state`, `kith-sched`, `nightly()`); `dev.kith.serve` runs `kith-uv run --script kith-serve-launch` only if a launcher is configured | A plist names the script path under `cli/current/share/kith/kernel/bin/`, so a deleted script means a job that fails every tick. Every job needs a rewrite or a stub (section 2.1). |
| Desktop host service | `apps/desktop/src/shared/host-service.ts`, `main/host-controller.ts` | `serviceRuns`, `plistSetsKernel` and `/\bHQ_BIN\b/`; `host-update.test.ts:120` builds a plist whose first argument is `kith-uv` | The app treats a plist that starts `kith-uv` as one `kith serve start` wrote and leaves it alone. Tests keep that behaviour. Note: the brief named `host-update.ts`; the file is `host-service.ts` (the test is `host-update.test.ts`). |

### 1.6 Other Python in the repo that is not the kernel

`scripts/voice-samples/`, `apps/ios/tools/`, `apps/tests/`, `serve/host/tests/fixtures/*.py`, `kernel/tests/difftest/{delegation-old,lifecycle-old,threads-old,selftest}.py` and `kernel/tests/kith-auth-props.py`. The last five are
test harness code (section 4). None of these is retired by this plan, except the difftest `*-old.py` ports of old behaviour,
which stay as frozen references.

## 2. Retirement order

Rules for every step.

1. **A step is a release, not an edit.** Deleted files are absent from the next `cli/<version>/`; the previous folder stays
   (`KITH_KEEP_VERSIONS`). A step never edits an installed kernel in place.
2. **Gate = the cutover module is switched on every Mac that will take the release**, checked at install time (section 3.2),
   plus the verb switch (`core-switched.txt`) where the step only moves verbs.
3. **Stub before delete.** A deleted script that a plist, a hook, an old host or a user's muscle memory names is replaced
   in the same release by a three-line `sh` stub that execs `kith kernel <verb> "$@"` (or prints one line and exits 3).
   Stubs live one full release (the window, section 3.1) after the last known caller is gone. This is the verb-level
   twin of `kith_tombstone.py`.
4. **Tag before the first deletion.** `git tag python-kernel-final` on the last commit that has every Python file. Undo of any
   step is "install the previous release folder", and "restore from the tag" for a source checkout.
5. **A step run twice is harmless.** Each step is a PR that deletes files (a second apply finds nothing) plus, where a
   Mac-side action is needed, a `kith migrate`/installer action that is idempotent by the cutover framework's own rules
   (`step` on a switched module re-records and re-tombstones; `rollback` on an unswitched module clears the record).
6. **Per-step behaviour in the three states.** Every step lists what happens on a Mac where its module is *unswitched*,
   *switched* and *rolled back* (a rolled-back module is an unswitched one that still has the old state files and a
   `rolling-back` or absent switch record; `kith migrate status MODULE` tells which).

Unless a step says otherwise: **unswitched or rolled back = the Mac is not offered the release** (installer refuses, the Mac
stays on the old `cli/<version>`, `kith doctor` says why); **switched = proceeds.**

### 2.0 Step 0: groundwork (no removal)

- Tag `python-kernel-final`. Write `kernel/lib/requires-switched.txt` (new, shipped with each release, one module per line)
  so the installer can ask the new `kith kernel cutover status` which modules are switched on this Mac. `kith doctor` and
  `kith migrate status --all --json` list per module and per Mac. No file is removed.
- Gate: none. Undo: delete the new file. Repeat: yes.
- The routine to know which Macs exist is the **list of owner Macs** (open decision 1).

### 2.1 The steps

| # | Gate: module and meaning of "switched" | Removes or tombstones | Undo | Unswitched / switched / rolled back |
|---|---|---|---|---|
| **R1** Leaves with Rust twins that Python never needs to fall back to | No cutover module. Verbs `engines`, `paths`, `launch`, `settings` of `core-switched.txt`, plus `readfence` (launch picks Rust when it knows a binary). "Switched" = the Mac's `kith` program lists the module in `kith kernel version` (every Mac on the floor release F, section 3.1). | `hq_paths.py`, `hq_settings.py`, `kith_engines.py`, `kith_readfence.py` and the `python3_abs` fallback in `launch/settings.rs`; the `KITH_CORE=python` branches for these verbs in `bin/kith`. Stub: none (no external caller). | Reinstall the previous `cli/<version>`; or restore from the tag. | Unswitched (no or old kith program): not offered the release. Switched: runs. Rolled back: n/a (no module). |
| **R2** Per-verb switched stores that carry no state | `cost`, `pkg`, `project-id`, `settings` lines present and the program on F. For `cost`: add `tag`, `record` to Rust first (the SessionStart hook and the ledger's only writer). For `pkg`: only `resolve` exists; **the script stays until its other verbs are ported or declared dead (R8)**, so R2 deletes only the fallback branch of `resolve`. For `project-id`: port the guest branch first (run-socket client) then delete `bin/project-id`. | `bin/project-id` (after its guest branch), `kith_cost` and `kith_cost_ported` in `kith-core.sh`, `kith-cost` after `tag`/`record`/reports are Rust. Hook `kith-cost tag` becomes `kith kernel cost tag` in `launch/settings.rs:774`. Stubs: `bin/kith-cost`, `bin/project-id` (forward). | Previous release; stubs forward, so a rollback of the Rust half is the same. | Unswitched: stays on old release. Switched: runs. In-flight run keeps the old hook path, which is now a stub that forwards: harmless. |
| **R3** `accounts` | Module `accounts` switched (`step accounts` done: state.json is a tombstone, the core is the only writer). On F: every Mac that runs agents. Kai's Mac: switched 2026-11-16. | `lib/kith_accounts.py`, `bin/kith-account`, the `account-state` fallback in `bin/kith:175`, `kith_accounts.py backups-json\|accounts-json` calls (`bin/kith:331-332`) once the chain verbs answer. Stub: `bin/kith-account`. | `kith rollback accounts` (Rust, writes state.json back) **then** the previous release. Rolled back state files are readable only by that older release. | Unswitched: run `kith migrate step accounts` (offered by the updater, section 3.2); refused if it refuses. Switched: proceeds. Rolled back: treated as unswitched, so the release waits. |
| **R4** `authority` | Module `authority` switched: `auth.json` is a tombstone (`kith migrate step authority`, `KITH_CORE_AUTHORITY` on, M2-24). On every Mac whose people or bots use the owner's agents. Kai's Mac: switched. | `bin/kith-auth` and its `hq-auth` link, `kith-people`, `kith-door` and `lib/kith_door.py`, `kith-allow`, `kith-helper-allow`, `lib/kith_guard.py`, `kith_allowed.py`, `kith_jobs.py`; `kith-permit` once the hooks and MCP command point at `kith kernel permit` (`settings.rs:977-984`, `mcp.rs:14`, `assemble.rs:1392,1425`). Ports first: Rust CLI for every `kith-auth` shape the callers use (the exit-3 shapes become real core calls or are declared dead with the callers named in their M2-23 lift list entries); `kith-allow --derive/--add-json`; the door process side. Stubs: `kith-auth`, `kith-permit`, `kith-door`, `kith-allow`, `kith-cards` (host calls by path), all as forwarders. | `kith rollback authority`, then the previous release. The model checks (section 4) re-run on the rolled-back state. | Unswitched: updater runs `step authority` (it already refuses on one-time secrets, a stale core import, or a changed file: the Mac then stays on the old release and says why). Switched: proceeds. Rolled back: held. |
| **R5** `claims`, then `transcripts` | Module `claims` switched on every Mac (#1010 lifted the refusal; Kai's Mac per main 2026-11-17, other Macs unknown; needs the host stopped, the stub installed (`kith-stub`), and every live claim importable). Then `transcripts` (`depends_on` claims; today it refuses until `kith-sessions record` stops being its writer). On every Mac that has terminal sessions or the host. | `bin/kith-sessions`, `kith-wake`, `kith-lifecycle`, `lib/kith_claims.py`, `kith_procstart.py`, `kith_lifecycle.py`, and the writes `kith-sessions record` makes. Ports first: `kith kernel threads` routed in `bin/kith`; `kith-sessions run` (the take-over note and exec) in Rust; the host's `wake.rs` calls the core's wake methods; `kith-lifecycle` (compact) in Rust. Hooks: `kith-sessions record` becomes `kith kernel threads record` (`settings.rs:776`). Stubs: `kith-sessions`, `kith-wake`, `kith-lifecycle` (the host calls them by path; old hosts too). | `kith rollback claims` (refuses while a claim is live unless `--force`; host stopped) then the previous release. `claims.tombstone` stays a note old Python takers read by existence. | Unswitched: **this is the long pole**: stepping needs the host stopped and the stub present; the updater cannot do it unattended. Release waits. Switched: proceeds. Rolled back: waits. |
| **R6** `schedules` | Module `schedules` switched (depends on `claims`): `sched/state.json` is a tombstone, the host's routines are jobs in `jobs.json`, the core's fire is the only scheduler. On every Mac. **Kai's Mac rolled it back 2026-11-16** because the core could not fire (M4-5): that Mac is *rolled back* here until `core-schedules` lands and `kith migrate step schedules` passes the four-fact gate in `schedules_gate.rs` (#1011), after which it is stepped again. | `bin/kith-sched`, the `kith schedule` dispatch, `kith_launchd.py` reload helper (if `kith-state` has moved), the installer that writes `dev.kith.sched`. The plist `dev.kith.sched` is removed by an idempotent installer action; the core fires from the host daemon. Stub: `kith-sched` (forward, or print the moved note and exit 3). | `kith rollback schedules` (host stopped) writes `state.json` and the routines back; then the previous release; `kith-sched --install` re-adds the job. | Unswitched/rolled back: release waits; Kai's `dev.kith.sched` is not installed anyway (docs/codebase-map.md). Switched: proceeds. |
| **R7** The run path | Verb switches, no module state: `launch` (done), the interactive tail, `adapters/harness.py`, `kith-ask`, `kith-recall`, `kith-memory ls\|search`, `kith-acp`. Gate: each verb in `core-switched.txt` with 5 green main-merged corpus runs (core-design section 11) and its golden frozen first. R5 and R4 are done (the claim, the permit and the hooks are already Rust). | `kith-ask` and `kith_trajectory.py`, `kith_recall.py`, `kith_runsocket.py`, `kith-recall`, `kith-memory`, `kith-acp` and the shim, `harness.py`, the Python in the bash launcher's tail, `kith_facets.py`, `kith_host.py`, `kith_runenv.py`. Ports first: each of these has a golden or difftest corpus (`asks.json`, `delegation.json`, `sessions.json`, `lifecycle.json`); freeze first where none exists (`kith-memory`, `kith-recall`). Stubs: `kith-recall`, `kith-memory`, `kith-ask` (hooks, the host's tidy and the permit helper call them by path). | Previous release. | Verb-level: Macs on F are switched by definition; older programs fall back to the previous release's Python. |
| **R8** Operator tools | Ports or drops, one PR per script: `kith-doctor`, `kith-info`, `kith-init`, `kith-state` (and `dev.kith.state`), `kith-remote`, `kith-self-update`, `kith-skills`, `kith-scratch`, `kith-project`, `kith-group`, `kith-send-to`, `kith-connector`, `kith-keys`, `kith-who`, `kith-bug`, `kith-chain`, and `kith-pkg`'s remaining verbs. A verb with no caller in the repo, no hook and no skill is a drop candidate (open decision 4). Gate: `kith doctor` green on each owner Mac on the new binary. | The scripts. Stubs: the ones the host or a plist calls by name (`kith-send-to`, `kith-cards`, `kith-state`). | Previous release. | A Mac on the floor release has the Rust verb or the stub forwards it. |
| **R9** The seams | Gate: R1-R8 released and every owner Mac on a release that has no Python caller. Then, in one release: `bin/kith` bash dispatch becomes the Rust `Route::Kernel` table in `serve/host/src/kernel.rs`; `kith migrate`/`kith rollback MODULE` dispatch moves from `kith-migrate` to `kith kernel cutover` directly; `kith-core.sh` and `core-switched.txt` retire (every verb is unconditional Rust, so "remove the line" is no longer a rollback); `kith_tombstone.py`, `kith_paths.py`, `kith_launchd.py`, `kith_migrate.py` (the rename steps stay until the rename is closed: see Edge cases), `kith-serve-launch` (section 5). `scripts/install.sh` loses its `python3` calls; the Homebrew formula loses `depends_on "python@3"`; `stage-kernel.sh` stops copying `kernel/bin` and `kernel/lib`. | The last Python; the stubs at the end of their window. | Previous release folder. `kith rollback MODULE` is Rust and needs no Python. | A Mac not on F is refused by `MIN_CLI` (section 3.1). |
| **R10** Retire the importers and the format readers | Gate: a **state-format floor**: every owner Mac has stepped every module and two releases have passed (open decision 5). | `core/runtime/src/import/*` (accounts, asks, authority, claims, schedules, transcripts, wake) and the `export_back` halves of the modules; tombstone readers in Rust. | Previous release plus tag. | Not applicable: no Mac is unswitched by definition. |

R1-R3 can overlap. R4 must precede R5 (the claims taker calls `kith-auth resolve`). R5 precedes R6 (`depends_on`).
R7 follows R5 because the run path's last exec is `kith-sessions run`. R9 is last by construction.

### 2.2 What "switched" must mean, by module

| Module | Switched means | Checked by | Today |
|---|---|---|---|
| `accounts` | `state.json` replaced by a tombstone; `accounts_core_writes` on; `kith account out\|clear\|usage` write core rows | `kith migrate status accounts` (switch file `state=switched`), marker in core.db, tombstone present | Kai's Mac switched 2026-11-16; other Macs: unknown, to list in Step 0 |
| `authority` | `auth.json` is the core's tombstone; Keychain items sealed into the envelope; `KITH_CORE_AUTHORITY` on and the gate passes | switch file, `authority::switched()` (host reads the tombstone by fingerprint), `cutover.mayServe authority` | Kai's Mac switched; the people-admin writes are app-only there |
| `claims` | `live/terminal/claims.tombstone` down, the claim files imported; host turns in Writer mode (`terminal/core_writer.rs`) | `cutover.mayServe claims`, `kith_claims.switched` | #1010 lifted the refusal; the 2026-11-16 log line says no Mac had stepped it, main reports it switched on Kai's Mac on 2026-11-17: confirm with `kith migrate status claims` |
| `transcripts` | The registry's rows are the core's; `kith-sessions record` no longer the writer | switch file; `depends_on claims` | Unswitched; no Mac. `core-transcripts` is working on it |
| `schedules` | `sched/state.json` tombstoned; routines are jobs; the core's fire is the only scheduler; host reads the gate at start | `schedules_gate.rs`: the build runs fired turns, depends on claims, serves `schedule.tick\|runNow\|fire`, and launchd runs the right program | Rolled back on Kai's Mac 2026-11-16; refused on builds that cannot run fired turns (#1011) |
| verb-level (`paths`, `engines`, `launch`, `settings`, `pkg`, `project-id`, `cost`) | the line is in the installed kernel's `core-switched.txt` and the program lists the module | `kith doctor` prints `core: rust (modules: ...)` | Switched everywhere the kernel and program are current; Macs with an old or missing program silently run Python |

## 3. Macs not yet switched

### 3.1 Who they are, and the window

- **Kai's Macs** (the main Mac, the Mac mini named in the agent notes): switched state differs per module (section 2.2).
  Step 0 lists them; open decision 1.
- **Invited people** do not run a kernel. They use the owner's host through the app or the web client (`docs/invited.md`);
  their runs are the owner's Python and Rust on the owner's Mac. They are never "unswitched Macs". The exception is anyone
  who installs their own kith and is invited to someone's agents; there is no such Mac in the repo's tests or docs to
  confirm, so the plan treats it as possible and lets the version gate (below) catch it.
- **Old desktop builds**: they bundle an old kernel with Python and install it as the CLI when the installed one is below
  `declared.cli`; the host binary inside them calls `kernel/bin/kith-auth` and the others by path. iOS builds speak only to
  the host (protocol level, `required_protocol`), so an old iOS build is a host-compatibility question, not a Python one.
- **Macs where a module was rolled back** (Kai's, schedules, 2026-11-16).

**Compatibility window.** Default (core-design Q2): the Python for a module is deleted **two releases after** the release
that first lets every owner Mac switch it, and only after `kith doctor` reports the program and the switched module on each
owner Mac. In release terms: release N ships the last port; N+1 starts the stubs; N+2 deletes the script and keeps the
stub; N+3 removes the stub. Two things end the window early or late: the owner list (decision 1) all green ends it; any
Mac on an older release holds it open (it keeps its `cli/<version>` and the Python in it).

**The gate in code.** Three existing checks, composed:

1. `MIN_CLI` (`serve/host/src/kernel.rs:172`, `cli_status`) is the host's own floor: a host refuses a CLI older than its
   major.minor. The first Python-free release raises the major.minor, so a new host on an old Python CLI shows "Kith needs
   the kith CLI X or newer" and the app's `ensureCli` installs the bundled one. This keeps a new host from running on a
   CLI that still wants state in the old form, and the reverse is the stub: an old host on a new CLI still finds
   `bin/kith-auth` and the others (as forwarders) for the length of the window.
2. `kernel/lib/requires-switched.txt` (new, Step 0): `scripts/install.sh` runs `<new>/bin/kith kernel cutover status
   --require ...` against the machine folder before it flips `cli/current`. A Mac whose required module is not switched
   stays on the old folder and prints the one step to run. Nothing is stepped automatically ("never automatic" in
   `cutover/modules/mod.rs`); for `accounts` and `authority` the installer offers `kith migrate step MODULE` as the next
   line.
3. `kith doctor` and `kith migrate status --all --json` as the owner's checklist.

### 3.2 Rollback once the Python files are gone

Today's rollback is "remove the module's line from `core-switched.txt`", or `kith rollback MODULE` (Rust) when state
moved. Both need the Python still on disk. After a step's release:

- **The release is the unit of rollback.** Point `cli/current` back at the previous `cli/<version>` (the installer keeps
  three; `ln -sfn` today, a `kith update --back` verb is a small addition of the `retire-python` manager, not a rule here).
  The previous folder still has the Python. For a module that moved state, run `kith rollback MODULE` **first** (it is Rust,
  host stopped, claims released), so the files the old release reads are written back; then flip the link. Order matters:
  flipped first, the old release meets a tombstone and refuses (fails closed), which is safe but leaves the Mac down until
  the rollback runs.
- **The tag.** `python-kernel-final` and the per-step commits make a source checkout restorable. A package user restores
  from the retained folder or by `KITH_VERSION=<old> sh install.sh`.
- **The stub window.** For N+2 the stubs make most failures visible and recoverable: a forwarder that cannot find the
  program prints one line (`this script moved to the kith program: kith update`) and exits 3, so a hook or plist that
  still names an old path fails loudly, not silently.
- **After the window**, `KITH_CORE=python` and `core-switched.txt` no longer exist; a Mac with a rolled-back module and a
  Python-free release is not a supported state. This is why the installer gate exists.

### 3.3 Hooks and plists are the sharp edge

A pinned package (`~/.kith/store/<name>@<version>/`) carries no hook command of kith's: kith writes its hooks at launch
(`settings.rs`). An agent's own `.claude/settings.json` or `role/settings.json` may name `kith-*` scripts. Step 0
includes `grep -rn "kernel/bin\|kith-" agents/*/role/settings.json` on each owner Mac (the repo has no role repos), and
the `kith-doctor` check for any launchd plist whose `ProgramArguments` names a kernel script
(`kith-doctor:460` already inspects `kith-uv` plists). Plists that name a deleted script keep running the stub until the
stub is removed; `kith-state` and `kith-sched` rewrite their own plists on `--install`/`nightly` and write
`stable_kernel()/bin/<script>` paths, which the release keeps valid through the stubs.

## 4. Tests and model checks

### 4.1 kernel/tests (104 entries) and CI

CI runs them in `kernel` (three shards, lines 343-436 of `.github/workflows/ci.yml`), `host-build` (difftest and the
switch checks, 752-767) and a few macOS-only ones that `kernel/tests/ci-exempt.txt` lists (`kith-sessions-check`,
`kith-permit-check`, `kith-install-layout-check`, `kith-accounts-check`, `kith-backup-check`, `kith-resume-pick-check`;
`kith-allow-check` is marked BROKEN). `kernel/tests/kith-ci-names-check` fails if a check is neither in `ci.yml` nor in
`ci-exempt.txt`, so **a deleted check must come out of `ci.yml` and `ci-exempt.txt` in the same PR** (and its selftest
rows).

| Group (checks) | Exercises | Action | When |
|---|---|---|---|
| `kith-auth-*` (check, c2, json, lock, revoke, router, secret, embedded, core-mirror, mixed-race, props, props.py), `kith-keys`, `kith-grants`, `kith-helper-mcp`, `kith-people`, `kith-accounts-tombstone`, `kith-sched-tombstone` | `kith-auth`, `kith-people`, tombstone behaviour | **Port** the cases to Rust integration tests against the core (`core/runtime` tests, `serve/host` `api/*_tests.rs` already model many). Cases that only prove "Python refuses a tombstone" (`kith-accounts-tombstone-check`, `kith-sched-tombstone-check`, `kith-auth-core-mirror-check`) are **deleted** with the script: the Rust side's own tests (`cutover/tests.rs`, `schedules_gate_tests.rs`) cover the framework. `kith-auth-props.py` is a property test; port to proptest in `serve/host` or keep as a golden replay. | R4 (same PR as the script) |
| `kith-permit-check`, `kith-must-ask-check`, `kith-trusted-allow-check`, `kith-agent-allow-check`, `kith-agent-deny-secrets-check`, `kith-guest-helper-check`, `kith-guest-door-check`, `kith-guest-jobs-check`, `kith-guest-sandbox-check` | permit, guard, doors | Already have Rust twins (`core/kernel/src/permit/tests.rs`, `mcp_tests.rs`, golden `permit*.json`). **Port** the shell-level ones that run launchd and the macOS sandbox as host e2e (`serve/host/tests/`); delete the rest. | R4 |
| `kith-sessions-check`, `kith-sessions-core-check`, `kith-wake-check`, `kith-lifecycle-check`, `kith-held-check`, `kith-direct-lock-check`, `kith-threads-race`, `kith-terminal-flags-check`, `kith-resume-pick-check`, `kith-procstart-check`, `kith-load-check` | sessions, claims, wake, lifecycle, held terminals | **Port** to `core/runtime` tests (`terminal_tests.rs`, `direct_park_tests.rs`, `hostclaim_tests.rs`, `parity_tests.rs` in `wake/` exist). `kith-procstart-check` (Python and Rust write the same string) is **deleted** when the Python writer is. `kith-threads-race` is the race harness the thread-writers model drives: port as a Rust stress test. | R5 |
| `kith-sched-*` (cron-table, jobs, json, origin, person, prune), `kith-send`, `kith-call`, `kith-ask-check`, `kith-chain-check` | schedules, ask, chain | `kith-sched-cron-table`, `-json`, `-jobs` are frozen into `golden/sched.json` (43 cases) and `fixtures/routines-next.json` (132 rows) and replayed on the Rust trigger: **delete** with the script; keep the golden. The rest **port**. | R6, R7 |
| `kith-cost-*`, `kith-limit-check`, `kith-run-once-cost-check`, `kith-settings-switch-check`, `kith-account-state-switch-check`, `kith-cost-switch-check`, `kith-pkg-pid-switch-check` | the already-switched verbs | These are *switch* checks (Python vs Rust same answer). After the Python goes the comparison has nothing to compare to: **delete**; the difftest golden carries the frozen answer. | R1-R3 |
| `kith-pkg-*`, `kith-new`, `kith-market`, `kith-scratch`, `kith-footguns`, `kith-json-misc`, `kith-owner-launch`, `kith-launch-*`, `kith-harness`, `kith-agent-*`, `kith-unknown-agent`, `kith-summary` | CLI surface | **Port** as difftest golden cases first (those not yet in `golden/`), then as Rust CLI tests in `serve/host`. | R7, R8 |
| `kith-install-*`, `kith-self-update`, `kith-migrate`, `kith-launchd`, `kith-serve`, `kith-serve-launch`, `kith-info`, `kith-doctor`-adjacent, `kith-stub-install`, `kith-hooks.test.ts` | installer, launchd, self-update | The installer checks are shell and stay (they test `scripts/install.sh`). The Python-specific ones (`kith-launchd-check`, `kith-migrate-check`'s rename steps, `kith-serve-launch-check`, `kith-self-update-check`) go with their scripts. `kith-hooks.test.ts` tests the plugin's TSX and stays. | R8, R9 |
| `kith-core-boundary-check`, `kith-core-deps-check`, `kith-contract-check`, `kith-cutover-check`, `kith-difftest-check`, `kith-ci-names-check` | the Rust/Python boundary and the harness | `kith-core-boundary-check` keeps the core "sealed": it must be rewritten to forbid *new* `python3` spawns instead of Python importing Rust. `kith-cutover-check` and `kith-contract-check` stay. `kith-difftest-check`: its OLD side is `KITH_CORE=python`; after R9 the harness runs `--golden` only. | R9 |

`kernel/tests/difftest/` (corpus of 29 cases, `golden/` 24 frozen files with `facets/`): the corpus's OLD side is the Python.
`kith-difftest --golden golden` was built to "survive the day Python is no longer there" (its README). Before a script is
deleted, **its corpus must be frozen as a golden in the same PR as the last Python commit**: `account-state`,
`accounts*`, `cost`, `delegation`, `engines` (not golden: Python prints "needs the kith program"), `json-*`, `launch*`,
`paths*`, `people`, `permit*`, `pkg-resolve`, `project-id`, `room-ledger`, `run-once`, `secrets-read`, `seed`,
`settings*`, `threads` still need their record where `golden/` lacks one (`golden/` has: account-state, accounts-core,
accounts, asks, auth-invite-bot, cost, delegation, json-auth, lifecycle, people, permit-mcp, permit, pkg-resolve,
project-id, room-ledger, sched, secrets-read, seed-auth, sessions, settings-entry, settings, threads, wake). Missing and
needed: `json-misc`, `json-pkg`, `json-sched`, `launch*`, `paths*`, `run-once`, `engines`. The `*-old.py` files
(`delegation-old.py`, `lifecycle-old.py`, `threads-old.py`) are frozen references for the golden recorder and stay.

`host-kernel-tests.txt` and `scripts/ci/host-kernel-tests.sh check`: host unit tests that read the kernel or run the real
`kith-auth` through the api `Rig` (`serve/host/src/api/rig.rs`, `guard_props.rs`, `roles_props.rs`, `botguests_props.rs`).
At R4 the Rig must run a core instead of the script: **port** (it already has `*_core_tests` neighbours).

### 4.2 The model checks (TLA+ / TLC) and the skills

The models are language-independent; what names Python is the **drift list** (`verification/*/drift.sh` `FILES`), the CI path
filters in `ci.yml` (lines 144-300) and each skill's trigger text. Python files in them:

| Model / skill | Python it names (drift.sh, skill text) | Rust it already lists | Action at the step |
|---|---|---|---|
| `thread-writers` (`verification/threads`) | `bin/kith`, `kith-lifecycle`, `kith-sessions`, `kith-wake`, `lib/kith_claims.py`, `lib/kith-core.sh` | `core/runtime/src/conversations`, `terminal.rs`, `serve/host/src/terminal/` | **R5**: replace each Python entry by its Rust owner in `FILES`, re-run `run.sh`, keep the model. The row for "the child execs claude (same pid)" stays tied to the stub (note in one-bot-model.md, P4/P10). |
| `writer-lock` (`verification/writer-lock`) | `bin/kith`, `kith-sessions` in the skill text | `core/kernel/src/procstart.rs` and the holder/stub | **R5**, same. |
| `revoke-access` (`verification/revoke`) | `bin/kith-auth` (`may`, `bot_may`, `cmd_person`), `bin/kith` (helper spawn), `lib/kith_door.py` (`DoorStart`, `DoorSweep`), `kith-permit ask_helper` | `core/runtime/src/authority/`, `core/kernel/src/permit/{helper,door}.rs`, `serve/host/src/remote/*` | **R4**: the model is already phrased over the core and host actors; drop the Python entries, re-sync. |
| `secret-cards` (`verification/secrets`) | `bin/kith-auth` (`set --stdin-json --grant`, `grant`, `keys`, `revoke --profile`) | `core/runtime/src/cards/`, `secrets/` | **R4**. |
| `schedule-fire` (`verification/schedule-fire`) | `bin/kith-sched` (tick, catch-up, paused skip) | `core/runtime/src/schedules/` | **R6**: the model's subject is already the core's fire; drop `kith-sched`. |
| `invitee-access` (skill) | `kernel/bin/kith-auth` (verbs), `kith-permit ask_helper` | `serve/host/src/api/roles.rs`, `core/kernel/src/permit/` | **R4**. |
| `run-tokens` (skill) | `kernel/bin/kith-recall`, `kernel/lib/kith_runsocket.py`, `kith-memory`, `project-id` guest branch | `serve/host/src/runauth.rs`, `core/runtime/src/runsock.rs` | **R7**: the model's guest-client half becomes the Rust client; re-run its property tests. |
| `holder-lease`, `rooms-with-people` | none | n/a | no change |

CI jobs: `thread-writers`, `thread-writers-slow`, `writer-lock`, `revoke-access`, `secret-cards`, `rooms-with-people`,
`holder-lease`, `schedule-fire` (lines 471-640). Each runs when its `changes` filter matches; **a deleted Python path left
in a filter is harmless, but a path left in `FILES` makes `drift.sh` report "file missing"** (read each `drift.sh` before
the step; the revoke one lists `kernel/lib/kith_door.py` and the host files it must compare). Delete Python entries and
re-run the model in the same PR as the script, as the skills ask ("the relevant property checks green").

## 5. Third-party agent tools and `kith-uv`

What the repo shows: `kernel/agents/` holds only `scratch` (no tools). Agents' tools live in their own role repos
(`agents/<name>/role/tools/`, not in this checkout). `docs/codebase-map.md` names two that matter: devops'
`role/tools/monitor.py` (`dev.kith.monitor`, KeepAlive) and house's `role/tools/house.py remind --send`
(`dev.kith.house`, 08:00), both launched through `~/.kith/bin/kith-uv` (`ProgramArguments`: `kith-uv run --quiet --script
<tool> ...`). I could not read the role repos from here: the next paragraph is inference from the codebase map, the plist
writers and `kith-doctor`, not from the tools themselves.

These tools are agents' own. They need a Python (and often uv's script dependency resolution), not the kernel. Retiring the
kernel's Python does not touch them, and the kernel's other uses of `kith-uv` are only: `kith-state`'s plist, `kith-sched`'s
plist, the host service when a launcher is configured, and `kith-doctor`'s "stuck in kith-uv means lost Full Disk Access"
check.

Why `kith-uv` exists: launchd credits file access (Full Disk Access, external volumes) to the program it starts. A repo on an
external disk (Kai's: `/Volumes/Macos 1TB/kith`) needs a launcher that holds the grant, and `uv` is a stable binary whose
path does not change when kith updates. (Inference: a TCC grant is keyed to the path and signature of the binary; the
`kith` program is replaced at every update, so a grant given to it would not survive. I did not test that; `docs/moving-from-hq.md`
records only that "Full Disk Access is per path" and that the grant must be given again after the rename.)

| Option | What it means | Cost |
|---|---|---|
| **A. Keep `kith-uv` as the FDA launcher for agents' tools and the host; stop the kernel depending on its Python** | The file `kith-uv` and `kith_paths.launcher()` stay. `kith-serve-launch` (Python, `uv run --script`) is replaced by a Rust one-file launcher with the same signal behaviour, or the host runs behind `kith-uv` as a plain program: `kith-uv` does not need to be `uv`; it needs to be a binary with the grant. | Small. No grant to redo if `kith-uv` is not replaced. |
| B. Give the grant to the host binary and drop `kith-uv` for the kernel; keep `kith-uv` only for agents' tools | Needs the grant to survive host updates (signing identity or a stable copy), or a re-grant each update. | A repeated manual step; a recurring "kith can't read my agents" failure. |
| C. Remove `kith-uv` everywhere | Agents' `dev.kith.monitor`, `dev.kith.house` and any `dev.kith.*` plist naming it stop; the FDA grant moves to something new; `kith-doctor`'s check, `host-service.ts`'s "started by kith" test and `host-update.test.ts`'s plist fixture change. | Highest; breaks third-party agents' installers. Not recommended. |

**Recommendation: A.** Keep `kith-uv`; document it as "the Full Disk Access launcher agents' tools run behind", not as a
kernel dependency. Retire `kith-serve-launch`'s Python (R9) by making the launcher a small Rust binary (or by letting the host
accept `launcher` as a plain program). Do not rename or replace `kith-uv`. `kith-init` keeps checking for `uv` only if an installed
agent needs it (`kith-init` reads `agents.json`). If `kith-uv` went anyway, every third-party agent's plist and installer would
need regenerating, and the Full Disk Access grant would be given again by hand on every Mac.

## 6. Edge cases (every line filled)

- **States.** Each cutover module can be unswitched, switched or rolled back on a Mac; the table in 2.1 says what each step
  does in each. Summary: a step never runs for a Mac that is not switched for its module; the installer holds that Mac on its old
  release and names the one command to run.
- **Existing features.**
  - Hooks written by the launch (`kith-cost tag`, `kith-sessions record`, `kith-room-ledger`, `kith-permit denied|once`, the
    `kithpermit` MCP server) change in the launch's Rust, one step at a time. A run in flight keeps its old command; the stub
    forwards.
  - Agents' own `.claude/settings.json` or role hooks that name `kith-*`: Step 0 greps them per Mac; the stub covers them
    for the window.
  - The host's subprocess calls by path: stubs for `kith-auth`, `kith-wake`, `kith-door`, `kith-send-to`, `kith-allow`,
    `kith-cards`, `kith-recall`, `kith-sched`, `kith-sessions`, `kith-lifecycle`, `kith-memory`, `project-id`, `kith-pkg`. R4 and R5 also change the host to call the core directly, so the stubs are only for old hosts.
  - `apps/desktop/src/shared/host-service.ts` and `main/host-controller.ts`: unaffected by the kernel steps; R9 touches the
    `kith-serve-launch` plist only if the launcher changes (it does not under option A).
  - `hq-*` links: they are symlinks to `kith-*`. They survive as long as the `hq` name does (`docs/moving-from-hq.md`); with the
    scripts gone they point at stubs. Their removal belongs to the hq rename work, not this package.
- **Repeats.** Per step in 2.1 and rule 5. The cutover framework's `step`, `rollback` and `status` are idempotent under the
  core.db lifetime lock; deleting files twice is a no-op.
- **Old data.** `state.json`, claim files, sched `state.json`, `auth.json`, `live/ask/*.reg`, wake queue files, and the
  session registry are read by `core/runtime/src/import/*` (accounts, asks, authority, claims, schedules, transcripts, wake).
  Those importers must outlive every Python writer, and after R9 they are the only readers of the old formats: keep them
  until R10's state-format floor (open decision 5). A Mac that updates past a Python writer without stepping would have
  state nobody reads; the installer gate prevents it.
- **Invitee Macs and old app builds.** Section 3.1.
- **The `hq` to `kith` rename.** `kith-migrate` and `lib/kith_migrate.py` (900 lines) hold two things: the rename and the
  cutover forwarder. This plan removes only the forwarder (R9). The rename code stays until `~/.hq` has no users; separate
  work.
- **Windows.** Python removal is what lets an owner run on Windows ("Gets: no Python in the run path; Windows owner use once the
  hook commands are Rust", one-bot-model M5). Out of scope here; R4/R5 hook rewrites are the prerequisite.

## 7. Work the plan assumes, by owner

| Needs | Owner today | Plan step it unblocks |
|---|---|---|
| Transcripts stepped (module `transcripts`) | manager `core-transcripts` (#747, #794 landed the module; the rest is in flight) | R5 |
| Schedules re-stepped and the core fires | manager `core-schedules` (#743, #762, #1011, #1015) | R6 |
| Followups of the rebuild (open SHOULDs, the `kith kernel` routing of `permit`, `threads`, `room-ledger`, `people`) | manager `rebuild-followups` | R2, R4, R5 |
| Claims stepped on every Mac | #1010 (lifted the refusal); Kai's Mac per main; other Macs unknown | R5, R6 |
| Rust twins for `kith-cost tag/record`, `kith-sessions run`, `kith-lifecycle`, `kith-allow --derive`, `kith-memory ls/search`, `kith-recall` client, `kith-pkg` other verbs, `kith-info`, `kith-doctor`, `kith-init` | none yet | R2, R5, R7, R8 |

`gh pr list --state all` on 2026-11-17 shows the landed prerequisites above and no open PR from the three managers yet;
names here refer to their agent names, not branches.

## 8. Open decisions for Kai

Each has a recommended default; the `retire-python` manager proceeds on the default and says so in its PR.

1. **Which Macs are in the gate?** The installer gate needs the owner list (Kai's Mac, the mini, any other). Options:
   (a) the Macs Kai names, checked by `kith migrate status --all` on each; (b) any Mac that runs `kith serve`; (c) no list: the
   release gate is per-Mac at install time only. **Default: (a) plus the per-Mac install gate.** Invitees are not Macs in
   the gate (they run no kernel).
2. **Window length.** Options: (a) two releases after the last Mac switched (core-design Q2); (b) one release; (c) until `kith
   doctor` is green on the owner Macs, no release count. **Default: (a), ended early only by (c) being green on every owner
   Mac.**
3. **Stub lifetime.** Options: (a) stubs live through the window then go in R9; (b) stubs forever (a three-line file per verb);
   (c) no stubs. **Default: (a).** (c) risks old hosts and plists; (b) keeps dead names.
4. **Port or drop the long tail.** Candidates to drop: `kith-chain` (no caller outside CI), `kith-who`/`hq-who` (a
   `kith-who:*` allow rule in `settings.rs:715` only), `kith-keys`, `kith-skills`, `kith-bug` (only `kith-permit`), `kith-connector`
   (host `mcp.rs` names its own server). Options: (a) drop a verb with no caller in repo, hooks or skills; (b) port all; (c)
   ask per verb. **Default: (c) in a list in the R8 PR, decided before work starts.** There is no usage telemetry, so "no
   caller" means no caller in this repo.
5. **Keep the importers?** Options: (a) delete `core/runtime/src/import/*` at R10, two releases after the last Mac steps; (b)
   keep forever; (c) delete with R9. **Default: (a).** (c) strands a Mac that skipped releases.
6. **The schedule tick when the host is down.** Python's `dev.kith.sched` ticked every minute through launchd, independent of
   the host; the core's fire runs in the host daemon (`kith serve`, KeepAlive). Kai's Mac has no `dev.kith.sched` installed, so no
   change there. Options: (a) accept; (b) keep a launchd job that runs a core `schedule.tick` when the host is not up. **Default:
   (a) and say so in the release notes.**
7. **`kith-uv`.** Section 5. **Default: A (keep, decouple from the kernel).**
8. **Remove `python@3` from the Homebrew formula and the installer's Python at R9?** Options: (a) yes at R9; (b) keep for
   one more release as a safety. **Default: (a)**, because the installer's gate runs the new program, not Python.
9. **`kith update --back`** (flip `cli/current` to the previous version): add it in R0 so rollback is one command? Options: (a) yes; (b) document `ln -sfn`. **Default: (a).**

## 9. Architect review

Not yet run. The draft is pushed to branch `flow/python-retire-plan-doc`; the review follows and this section records the
verdict and what changed.
