---
name: registry-step
description: Model-checks (TLA+/TLC) the transcripts session-registry cutover, the rule that every session record a SessionStart hook makes ends up as the newest record where the readers read it (the file `$KITH_STATE/live/terminal/<sid>.json`; once switched also the core's session_records row), that the core never puts an older record over a newer file, and that write-ahead spool entries never erase each other. Use when a change touches kernel/bin/kith-sessions record (Files mode, Writer mode, the spool, the call, deleting its own entry), kernel/lib/kith_registry.py (switched, record, spool, drain), kernel/bin/kith-sched prune_terminal and its inbox drain, the core's session.record / reconcile / drain / prune (core/runtime/src/transcripts/, the daemon's 60 s pass in core/runtime/src/daemon.rs), core/runtime/src/import/transcripts.rs, core/runtime/src/cutover/modules/transcripts.rs (preflight, import, tombstone, before_tombstone re-read, cancel_step, export_back), or core/kernel/src/threads/registry.rs record. Also use to answer "can a session's record get lost or go backwards during or after the transcripts step?".
---

# Registry step: every hook's record ends up newest where readers read it

The transcripts cutover makes the core the only writer of the session registry. In Phase A the files stay; the core
keeps them as a projection of its `session_records` rows. Hooks in Writer mode spool first, call `session.record`,
and delete their own entry on an answer. The step, cancel and rollback move the Mac between Files mode and Writer
mode. Old-kernel hooks of running sessions keep writing files directly (architect MUST 1). This is about interleavings,
crashes and kills, so it is checked with TLA+ (TLC), not with tests. The interfaces are fixed by the task's contract
(manager core-transcripts-2, 2026-11-17, amendments A1-A5 from this model).

The model is `verification/registry/RegistryStep.tla`. This skill is the only documentation; there is no README.

## Run it

```
verification/registry/run.sh                       # all rows of checks.tsv
verification/registry/run.sh --show VARIANT PROP   # TLC's full output (the counterexample) for one row
verification/registry/drift.sh                     # which covered files changed since the sync commit
```

CI runs `run.sh` (job `registry-step`) on pull requests and pushes that touch `verification/registry/`, this skill,
or the covered code. `run.sh` exits 0 only when every row of `checks.tsv` (variant, kind, property, expected) came out
as expected; it is a copy of the holder-lease harness (pinned, sha256-checked `tla2tools.jar` v1.7.4 and Temurin JRE,
cached under `${XDG_CACHE_HOME:-$HOME/.cache}/kith-verify/`). `fixed` has 110,140 states; the liveness row is the slow
one (about 100 s). The whole table (29 rows) takes about four minutes on four workers. A row that changes exits 1:
read this skill, do not edit the table to fit.

## What is modelled

One session id. Three hooks of that session, one after the other (one Claude process per sid). Each hook is `new`
(today's kith-sessions with the Writer-mode code), `old` (a session pinned to an older kernel: writes `<sid>.json`
itself, no `recordedAt`, ignores the marker) or `bad` (a record the core refuses as invalid). Any hook can be killed
at any step. Keys stand for `recordedAt` (an old hook's key is its file's mtime). One counter hands them out, so a
larger key is a newer record.

- **New hook, Files mode:** writes the file, drains the inbox into place (only newer entries), re-checks the mode. If
  Writer mode turned on, it also runs the Writer steps.
- **New hook, Writer mode:** spool entry `inbox/<sid>.<recordedAt>.<pid>.json`, then the call. On `recorded`,
  `older` or `invalid` it deletes its own entry. On not-serving, no answer or a timeout, the entry stays.
- **Core:**
  - `session.record` is a newer-wins transaction, then the file write under the per-sid lock, then the answer after
    the rename.
  - The daemon reconciles and drains every 60 s, only while switched, racing live records.
  - The hourly prune removes the file and the row.
  - The core can crash at any step, including between commit and rename.
- **kith-sched, Files mode only:** prune and the inbox drain.
- **Step:**
  - preflight (inbox empty), then the switch file says stepping;
  - import (row := file);
  - tombstone (Writer mode on for hooks; the core does not serve yet);
  - before_tombstone re-read (row := file);
  - marker and switch (the daemon serves).
- **Cancel** before the switch puts the Mac back in Files mode.
- **Rollback:** the record says rolling-back and the daemon is stopped (hooks stay in Writer mode and spool), then
  `export_back` drains the inbox into the row and writes the row to the file, then the marker and record go.

Writer mode is `registry.tombstone` exists, or the transcripts switch file says more than `stepping` (switched,
rolling-back). Fair: a hook's own steps (not its start or the kill), the core's work, its restart, the 60 s pass,
kith-sched's hourly pass, and a step once begun. Not fair: starting a step, cancel, rollback, crash, prune.

| Property | Plain sentence | Harm |
|---|---|---|
| `Converges` (liveness) | Once things settle, the newest valid record any hook handed off (and no prune removed after it) is in the file, and once switched also in the row. The inbox is empty. | A reader sees an old pid or a missing session. `writer()` and resume pick the wrong run. |
| `NeverBackwards` | The core never replaces the file with an older record than the one it holds. | Same, until the next pass, if there is one. |
| `SpoolKept` | A spool entry nobody has taken (into the row, the file, or answered) is still in the inbox. | Two spools of one session erase each other. |
| `BadNeverListed` | An invalid record is never in the file or the row. | A record the core refused shows up anyway. |

## Findings (2026-11-17, taken into the contract as amendments)

`contract` is contract.md as first written. It fails `Converges` and `NeverBackwards`. Each fix alone is needed:

1. **`missing-oldest`:** a file with no `recordedAt` was "older than anything". Old-kernel hooks write none, so the
   reconcile rewrote their file from the older row, and MUST 1 never took them in. Fix: such a file orders by its
   mtime (ns).
2. **`project-row`:** session.record wrote the row over the file whatever the file held. Trace:
   - hook 1 spools k1 and calls, then is killed;
   - the next hook (old kernel) writes file k2;
   - the txn commits k1, and the file write puts k1 over k2.

   A late drain of an older entry does the same. Fix: the file write after the commit is the per-sid reconcile. A file
   newer than the row goes into the row; otherwise the row is written to the file.
3. **`export-different`:** export_back wrote the row "where different". That fails even with no old hook
   (`contract-no-old`):
   - a new Files-mode hook reads the mode before the tombstone, writes its file after the switch, and is killed before
     its re-check;
   - rollback then exports the older row over it.

   Fix: write the row only over an older file.

**Known window, documented, not closed (`blind-write` fails):** the core's look at the file and its rename are two
steps. This covers the record's file write, the reconcile, and the prune's unlink. An old hook that takes no lock and
writes in between loses that record. `fixed` treats the look and the rename as one step (`FileWrite = "atomic"`).
Closing the window in code would need an exchange-rename (`renameatx_np` RENAME_SWAP, `renameat2` RENAME_EXCHANGE)
and a check of what came out. The window needs an old-kernel hook of the same session in that millisecond.

Assumed, not modelled as a switch: the Files-mode drains (the hook's and kith-sched's) drop entries the core would
refuse as invalid.

Not modelled: several sessions (independent per sid), the content of a record beyond its key, the compact
`startedAt` rule, a second step after a rollback or cancel, kith-sched's own prune race with a Files-mode hook (as
today), Windows (refused by preflight), the readers' Phase B.

## Broken variants

| Variant | Switch | Fails |
|---|---|---|
| `contract` | 1-3 unfixed + blind writes | `Converges`, `NeverBackwards` |
| `contract-no-old` | the same with three new hooks | `Converges` (finding 3) |
| `missing-oldest` / `project-row` / `export-different` | one fix undone | `Converges` |
| `blind-write` | look, then a plain rename | `Converges` (the known window) |
| `no-write-ahead` | call first, spool on no answer | `Converges` (killed mid-call) |
| `one-name` | one spool name per sid | `SpoolKept` |
| `file-from-request` | the file from the request, not the row (MUST 2) | `NeverBackwards` |
| `no-reconcile` | no 60 s reconcile (MUST 1) | `Converges` |
| `ns-drop` | not-serving deletes the entry (MUST 5) | `Converges`, `SpoolKept` |
| `bad` | an invalid hook in the middle, no crash or prune | none (passes) |

The `Never*` rows are expected to fail. They show that each of these happens: a switch, a rollback, an old hook's
file newer than the row while switched, a crash between commit and rename, a core drain, a drain racing a live
record, a not-serving answer, a prune and an invalid answer. So a pass is not vacuous.

## Code to model

At the sync commit none of the Writer-mode code exists yet (P1 core, P2 Python, P3 module are being built against
the contract). The first PR that lands each part fills in this table and moves `SYNC`.

| Model action | Code (planned) |
|---|---|
| `HookStart`, `FilesWrite`, `FilesHookDrain`, `FilesRecheck`, `Spool`, `Call`, `Answered`, `TimeOut`, `Kill` | `kernel/bin/kith-sessions` record() via `kernel/lib/kith_registry.py` (switched, record, spool, drain) |
| `OldWrite` | an older kernel's `kith-sessions` record() (today's code at the sync commit) |
| `Txn`, `Project`, `ProjectWrite`, `Finish` | `core/runtime/src/transcripts/` session.record (daemon dispatch and embedded fallback) |
| `DrainStart`, `Reconcile1`, `CorePrune`, `RcFinish` | the daemon's start and 60 s pass, the hourly prune (`core/runtime/src/daemon.rs`, transcripts/) |
| `Crash`, `Restart` | the daemon or the embedded core dies, comes back |
| `SchedDrain`, `SchedPrune` | `kernel/bin/kith-sched` prune_terminal (Files mode only) |
| `Begin`, `Import`, `Tombstone`, `Reread`, `Switch`, `Cancel` | `core/runtime/src/cutover/modules/transcripts.rs` preflight, import (`import/transcripts.rs`), tombstones, before_tombstone, cancel_step |
| `Rollback`, `ExportBack`, `RollbackDone` | the framework's rollback, the module's export_back |
| (refuses under the marker) | `core/kernel/src/threads/registry.rs` record |

## When you change covered code

1. Run `verification/registry/drift.sh`. If it lists nothing, there is no work. If it lists files, read each diff and
   its commit message.
2. Decide for each change: it has no effect on the model; or the model no longer matches (a step split, a check
   moved, a new writer of the folder); or the promise changed on purpose. Update `RegistryStep.tla` and `base.consts`
   from the code, not from what makes a result pass. One step in code is one action.
3. Run `verification/registry/run.sh`. A row that differs is drift (replay the TLC trace against the code) or a bug.
4. A pass can have lost the ability to fail. Break the new check in a scratch edit and see `run.sh` fail.
5. Only then set `SYNC` in `drift.sh` and the sync commit below.

**Sync commit:** `48ed39f13b7c71814a96fb59ea3a8be71a64333a` (rust-rebuild; the model came before the code).

## Map of the folder

- `RegistryStep.tla`: the model.
- `base.consts`: the design's switches.
- `variants/*.set`: the overrides for each variant.
- `checks.tsv`: the table `run.sh` enforces.
- `run.sh`: the pinned tools and the table.
- `drift.sh`: the sync commit and the covered files.
