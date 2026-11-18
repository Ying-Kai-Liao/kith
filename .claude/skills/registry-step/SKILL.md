---
name: registry-step
description: Model-checks (TLA+/TLC) the transcripts session-registry cutover, the rule that every session record a SessionStart hook makes ends up as the newest record where the readers read it (the file `$KITH_STATE/live/terminal/<sid>.json`; once switched also the core's session_records row), that the core never puts an older record over a newer file, and that write-ahead spool entries never erase each other. Use when a change touches kernel/bin/kith-sessions record (Files mode, Writer mode, the spool, the call, deleting its own entry), kernel/lib/kith_registry.py (switched, order_key, valid, lock_at, spool, call, drain, sweep_temps), kernel/bin/kith-sched prune_terminal and its inbox drain, the core's session.record / reconcile / drain / prune (core/runtime/src/transcripts/, the daemon's 60 s pass in core/runtime/src/daemon.rs), core/runtime/src/import/transcripts.rs, core/runtime/src/cutover/modules/transcripts.rs (preflight, import, tombstone, before_tombstone re-read, cancel_step, export_back), or core/kernel/src/threads/registry.rs record. Also use to answer "can a session's record get lost or go backwards during or after the transcripts step?".
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
cached under `${XDG_CACHE_HOME:-$HOME/.cache}/kith-verify/`). `fixed` has about 1.15M states; the liveness rows (`Converges`, `StepFinishes`) are the slow
ones (about 2 min each). The whole table (49 rows) takes about 5 min here on 4 workers. A row that changes exits 1:
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
  - preflight, then the switch file says stepping (the inbox may hold entries: the re-read drains it);
  - import (mirrors the files: a row newer than its file is replaced, a row whose file is gone is cleared, `ImportClears`);
  - same_answer guards the tombstone (`TombGuard`): a row newer than its file or with no file stops the step, and a
    rerun imports again (`RerunImp`);
  - tombstone (Writer mode on for hooks; the core does not serve yet);
  - before_tombstone re-read: drain the inbox, take the files in, clear rows whose file is gone. A stale row refuses
    and takes the tombstone away (`prer`: stepping without it, hooks write files again);
  - marker and switch (the daemon serves).
  - A rerun with the tombstone still in place (`Rerun`) stays in Writer mode.
  - A second step may begin from Files mode (`files`, `filesc`), after a cancel or a rollback.
- **Cancel** before the switch puts the Mac back in Files mode (`filesc` when it left spool entries).
- **Rollback:** the record says rolling-back and the daemon is stopped (hooks stay in Writer mode and spool), then
  `export_back` drains the inbox into the row and writes the row to the file (`rbxe`: it took a spool entry), then the marker and
record go. Hooks keep spooling until the record is cleared; what they spool after the last drain waits for the next Files-mode
drain (the next session start on this Mac, or kith-sched's hourly pass: `kith_registry.drain` empties the whole inbox), the
gap the model has as `rbx` followed by Files mode.

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
refuse as invalid. Both validators run the same fixture files
(`kernel/tests/fixtures/registry-records/{valid,invalid}/`, contract A9), and drains skip dot-names and non-`.json` names.

Assumed by the model, required of the code (architect MUSTs on #1032, contract A6 and A7):
- **One order key, stamped everywhere (A6).** The model keys a file by its record, not by when it was written. That
  is only true if every file the core writes carries `recordedAt`: a row adopted from an unstamped file stores that
  file's `st_mtime_ns` as its `recordedAt`, and every projection, reconcile write and export_back writes it into the
  file. One `order_key` (recordedAt, else st_mtime_ns; then procStart; then arrival) serves the settle, the reconcile,
  the import, the re-read, export_back and both Python drains. Without it: a row adopted at mtime m is written back
  unstamped (mtime now), a hook's record k (m < k < now) commits, and the settle puts m back over k.
- **Row writes are conditional (A7).** `Project`, `Reconcile1` and `CorePrune` read and write the row in one step
  once, but `Txn` runs outside the per-sid lock, so in code a commit can land in between. Adopting a file is the same
  newer-wins upsert as the record's txn, and the prune deletes a row only where its key is unchanged. The model now has
  those steps split (a look under the lock, then the conditional write: `Project` and `ProjectAdopt`; `Reconcile1`,
  `CorePrune` and `RcFinish` with the look in `rc`), and `RowWrite = "blind"` (`blind-row-write`) is the broken variant
  that writes the row from what the look read.
- **The per-sid lock is a file lock (A8)** on `live/terminal/locks/<sid>.lock`, also taken by the Files-mode hook
  write and both Python drains, so the look-then-rename window above is left only to old-kernel writers.

Not modelled, harmless: an old-kernel kith-sched (after a downgrade) prunes whatever the mode; the reconcile writes
the file back from the row.

Not modelled: several sessions (independent per sid), the content of a record beyond its key, the compact
`startedAt` rule (the same-answer compare leaves `startedAt` out of a compact), more than two steps, Windows (refused by
preflight), the readers' Phase B. The step's `drain_once` before the re-read writes registry files from rows before the
switch (settle); the model has it as `Reread` taking the newest valid entry into row and file.

A record a Files-mode prune removed can come back through a stale spool entry once switched (a drain of an entry that
committed before a later record was pruned). That is the same as any record arriving late; it is why the model's guard is
the same-answer check at the step (`SwitchClean`, `StepFinishes`), not a "never resurrected" invariant.

## Broken variants

| Variant | Switch | Fails |
|---|---|---|
| `contract` | 1-3 unfixed + blind writes | `Converges`, `NeverBackwards` |
| `contract-no-old` | the same with three new hooks | `Converges` (finding 3) |
| `missing-oldest` / `project-row` / `export-different` | one fix undone | `Converges` |
| `blind-write` | look, then a plain rename | `Converges` (the known window) |
| `blind-row-write` | adopt or prune the row from what the look read (A7 undone) | `Converges` |
| `no-write-ahead` | call first, spool on no answer | `Converges` (killed mid-call) |
| `one-name` | one spool name per sid | `SpoolKept` |
| `file-from-request` | the file from the request, not the row (MUST 2) | `NeverBackwards` |
| `no-reconcile` | no 60 s reconcile (MUST 1) | `Converges` |
| `ns-drop` | not-serving deletes the entry (MUST 5) | `Converges`, `SpoolKept` |
| `bad` | an invalid hook in the middle, no crash or prune | none (passes) |
| `fixed-new` | three new hooks (no old-kernel hook) | none (passes) |
| `import-keeps-rows` | the import keeps a row whose file was pruned | `StepFinishes` (the same-answer guard stops the step for ever) |
| `no-guard` | the same, with no same-answer guard at the tombstone or the re-read | `SwitchClean` (it switches over the stale row) |
| `reread-refuses` | the import keeps the row, the tombstone is not guarded: the re-read's refusal is taken | none (passes `SpoolKept`, `NeverBackwards`, `Converges`) |

The `Never*` rows are expected to fail. They show that each of these happens: a switch, a rollback, an old hook's
file newer than the row while switched, a crash between commit and rename, a core drain, a drain racing a live
record, a not-serving answer, a prune and an invalid answer, `export_back` taking a spool entry, a Files-mode drain of what a
cancel left, a second step, and the re-read's refusal. So a pass is not vacuous.

## Code to model

The Python side (P2, #1041), the core (P1, #1040) and the module (P3) are in. The PR that lands each part fills in its
rows and moves `SYNC`.

| Model action | Code |
|---|---|
| `HookStart`, `FilesWrite`, `FilesRecheck` | `kernel/bin/kith-sessions` `record()`: `kith_registry.switched`, the Files-mode write of `<sid>.json` under `lock_at`, the re-check after the drain |
| `FilesHookDrain` | `kith_registry.drain` (and `sweep_temps`), called from `record()` in Files mode |
| `Spool`, `Call`, `Answered`, `TimeOut`, `Kill` | `kith-sessions` `record_in_core()`: `kith_registry.spool`, `kith_registry.call` (3 s), delete its own entry on `recorded`/`older`/`invalid` |
| `OldWrite` | an older kernel's `kith-sessions` record() (today's code at the sync commit) |
| `Txn`, `Finish` | `core/runtime/src/transcripts/record.rs` `call` (the daemon's dispatch and the embedded fallback) -> `record_in` -> `commit_as` (the newer-wins upsert in one transaction) |
| `Project`, `ProjectAdopt`, `ProjectWrite` | `transcripts/project.rs` `settle_with` under `with_lock`: the look at the file, then `commit_as(.., Tie::Strict)` (the conditional adopt), then the row read back and `write_atomic` (the rename; `ProjectWrite` is the known window, A4) |
| `DrainStart` | `transcripts/reconcile.rs` `drain_once` (each spool entry through `record_in`, then deleted; it sweeps dot-temps older than 300 s), run by `transcripts/tick.rs` `pass` at start and every 60 s (`daemon.rs` spawns it) |
| `Reconcile1`, `RcFinish` (adopt, write) | `reconcile.rs` `reconcile_once` -> `settle` (the same look and conditional adopt as above) |
| `CorePrune`, `RcFinish` (prune) | `transcripts/prune.rs` `prune_once`: judges rows by `(recorded_at, proc_start)`, deletes with `recorded_at = ? AND COALESCE(proc_start,'') = ?` |
| `Crash`, `Restart` | the daemon or the embedded core dies, comes back |
| `SchedDrain`, `SchedPrune` | `kernel/bin/kith-sched` `prune_registry` (Files mode only: `kith_registry.drain`) and `prune_terminal` (deletes under `lock_at`, re-checks age and pid) |
| `Begin` | `cutover::step` (any `Stepping` record or none) after `transcripts_preflight.rs` `preflight` (host stopped, not Windows, `kith-sessions caps`, `session_records` table); the inbox is not a refusal |
| `Import`, `RerunImp` | `import/transcripts.rs` `run_in` with `clear_gone` (`rows_without_file`), then `transcripts_answer.rs` `differences` (the framework refuses a step on any difference: `TombGuard`); a rerun imports again |
| `Tombstone` | `modules/transcripts.rs` `before_tombstone`: host re-probe, `tombstone::ensure(registry.tombstone)` |
| `Reread`, `Rerun` | `transcripts_answer.rs` `reread` (through the framework's `Module::reread`, after `before_tombstone`): `drain_once`, `import_files`, clear rows without a file, `record_diffs` (non-strict); on a stale row remove the tombstone and refuse |
| `Switch` | the framework: marker, then the switched record; the daemon's 60 s pass serves |
| `Cancel` | `transcripts_back.rs` `cancel` (`cancel_step`): remove the tombstone |
| `Rollback`, `ExportBack`, `RollbackDone` | the framework's rollback; `transcripts_back.rs` `export_back`: `drain_once`, `export_rows` (A3), `drain_once`, remove the tombstone last |
| (refuses under the marker) | `core/kernel/src/threads/registry.rs` `record` (refuses once `registry.tombstone` exists) |

Where the Python code differs from the model (the model allows more, so no action changes):
- The drain takes one entry at a time under that sid's lock, reads the mode once at its start, and leaves an entry
  whose lock is busy for the next drain. The model's drain is one atomic step that takes every entry.
- When the spool write fails (disk full, permissions), that one hook calls the core without its write-ahead entry:
  for it, the `no-write-ahead` variant applies (a kill mid-call loses that record).
- kith-sched's prune deletes under the per-sid lock and re-checks age and pid there, so its race with a Files-mode
  hook is closed (it was on the "Not modelled" list).

Where the core's code differs from the model (the model allows more, so no action changes):
- The re-read is three steps in code (drain, one transaction, the check) and one in the model. A writer that slips in
  between can only make a file newer, which is no refusal (Python's prune is off in Writer mode). Any failure of the
  re-read, not only a stale row, removes the tombstone: the model's `prer` branch.
- `prune_once` judges which rows are old and dead outside the lock, then re-checks inside it: the row by its whole key
  (compare-and-delete) and the file (a valid record of the sid whose key beats the judged one is left for the
  reconcile). The model's `CorePrune` judges and checks the file in one look. The code removes the file only when it
  deleted the row; the model unlinks the file even when the row stayed.
- The settle reads the file once, adopts through the conditional upsert, then reads the row back and renames. A commit
  after that read settles under the same lock after it, so the model's one `ProjectAdopt` covers both.

## When you change covered code

1. Run `verification/registry/drift.sh`. If it lists nothing, there is no work. If it lists files, read each diff and
   its commit message.
2. Decide for each change: it has no effect on the model; or the model no longer matches (a step split, a check
   moved, a new writer of the folder); or the promise changed on purpose. Update `RegistryStep.tla` and `base.consts`
   from the code, not from what makes a result pass. One step in code is one action.
3. Run `verification/registry/run.sh`. A row that differs is drift (replay the TLC trace against the code) or a bug.
4. A pass can have lost the ability to fail. Break the new check in a scratch edit and see `run.sh` fail.
5. Only then set `SYNC` in `drift.sh` and the sync commit below.

**Sync commit:** `553030268505e3fe9ab8386e763c90dff8074b81` (transcripts P3, the cutover module; P1 #1040 and P2 #1041 are in it).

## Map of the folder

- `RegistryStep.tla`: the model.
- `base.consts`: the design's switches.
- `variants/*.set`: the overrides for each variant.
- `checks.tsv`: the table `run.sh` enforces.
- `run.sh`: the pinned tools and the table.
- `drift.sh`: the sync commit and the covered files.
