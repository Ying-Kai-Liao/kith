---
name: holder-idle
description: Model-checks (TLA+/TLC) that a holder holding only unacked exit records, with no core connected, ends instead of staying until reboot, and that a returning core's claim for such a record still ends. Use when a change touches core/holder/src/holder.rs (should_idle_out, EXIT_KEEP, Config::exit_keep, watch_idle, idle_since, core_lost, drop_core, the ACK arm, reap, where an exit is recorded), core/holder/src/main.rs (the exit_keep plumbing), or where the core ends a claim the holder does not list (core/runtime/src/holder_link.rs open_claims and step 7 of reconcile, claims::is_reconciled, claims::end_gone's kind filter, conversations/reconcile.rs release_unlisted). Also use to answer "does a holder stay forever after an uninstall?" and "when does the core end a claim its holder no longer lists?".
---

# Holder idle: a holder with only unacked exits ends after a bound, and keeps its records

The holder keeps a process's exit record until a core acks it (`p::ACK`), and used to idle out only with an empty list and no
core (`watch_idle`). After an uninstall, a recorded exit was never acked and the holder never left (until reboot). The fix
(`Holder::should_idle_out`): with idle set and no core connected, the holder ends when either the list is empty and idle
time has passed, or every entry is an exit record and `exit_keep` (default `EXIT_KEEP`, 10 minutes) has passed since the
LATER of the newest exit and the link loss. It does NOT drop records: dropping one while the holder stays leaves the core's
claim held until the core restarts, because the core ends an unlisted claim only at the first hello of a core generation
(`open_claims`, guarded by `claims::is_reconciled`) or when a NEW holder instance answers (step 7 of `reconcile`,
`claims::different_holder`); a plain reconnect to the same instance ends nothing, and `end_gone` takes only `ending` and
`external` claims. A core that comes back after the holder left meets a new instance, and step 7 ends the claim (the turn
ends as "unknown": the exit code is lost). `supervise` (idle zero) never exits and keeps every record. A running entry or a
connected core blocks the exit.

Two models: `verification/holder-idle/WinKeeper.tla` (the W12 keeper model: writers, and that the holder ends) and
`HolderIdleGen.tla` (the architect's focused model of one claim across link drops, core restarts and holder restarts, with
the core's reconcile as the code runs it; rows `gen-*`). WinKeeper's `TermUnlisted` is an abstraction that ends an unlisted claim on every
link-up, which the code does not; the claim's end is checked only in `HolderIdleGen.tla`. This skill is the only documentation here.

## Run it

```
verification/holder-idle/run.sh                 # all rows of checks.tsv (about a minute)
verification/holder-idle/run.sh --show CFG      # TLC's full output (the counterexample) for one row
verification/holder-idle/drift.sh               # which covered files changed since the sync commit
```

CI runs `run.sh` (job `holder-idle`) when `verification/holder-idle/`, this skill, `holder.rs`, `holder_link.rs`, `conversations/claims.rs` or `conversations/reconcile.rs` change.
Tools are the same pinned, sha256-checked `tla2tools.jar` v1.7.4 and Temurin JRE as the holder-lease skill (`run.sh` is a copy
of its harness); each row of `checks.tsv` names a complete cfg in `cfgs/`.

## Rows

| Cfg | Expected | What it shows |
|---|---|---|
| `idle-today-live` (no `Expire`, core gone for good) | fail, `HolderIdles` | the terminal ends, its exit is recorded, the core goes away: the record stays and the holder never idles out |
| `idle-fix-live` (`Expire` = the holder ends with its records) | pass | the holder ends after the bound |
| `idle-fix-safe-windows`, `idle-fix-safe-unix` (`Expire`, core down then back) | pass | `OneWriter`, `ExactlyOnce`, `NoEndWhileLive`, `AnsweredDelivered` on both OSes (what each does to a leftover child differs) |
| `idle-broken-RecordBeforeSweep-unix` | fail, `OneWriter` | the exit is recorded while a child of the program still runs; the core acks and frees the claim; a host turn starts: two writers. Fails without any expiry |
| `gen-exit-live`, `gen-exit-idle` (`Mode = "exit"`) | pass, `ClaimEnds` / `HolderIdles` | the holder keeps the record and ends; a returning core meets a new instance and step 7 ends the claim |
| `gen-drop-live` (`Mode = "drop"`) | fail, `ClaimEnds` (broken, expected) | the holder drops the record and stays: a plain reconnect ends nothing and the claim stays held until the core restarts |

## Code to model

| Model | Code | Notes |
|---|---|---|
| exit record `texit`, written after the sweep | `holder.rs` `reap`: `proc.sweep()`, then the record (and `exited_at`) | **Keep the order.** It is what makes both the ack and the expiry safe. The comment at the sweep names `RecordBeforeSweep` |
| core link down | `drop_core` sets `core_lost` (the lease clock) | `core_lost` starts at the holder's start: a holder no core ever reached counts from there |
| `Expire` / `IdleExit` | `should_idle_out`, run by `watch_idle` every 100 ms | no core; empty list and `idle_since` older than `idle`, or only `held.exit.is_some()` entries and `max(newest exited_at, core_lost) + exit_keep` passed; zero `exit_keep` = no exit on records; zero `idle` = never |
| core's step 5 | `holder_link.rs` `open_claims`, once per core generation (`claims::is_reconciled`) | ends claims the welcome does not list, first hello of a generation only |
| core's step 7 | `holder_link.rs` `reconcile`, `claims::different_holder` | ends the old instance's unlisted claims when the holder instance changed |
| `end_gone` | `conversations/claims.rs` `end_gone`'s kind filter | takes only `ending` and `external` claims, so a running terminal claim ends only by an exit report, step 5 or step 7 |
| unlisted claim's effect | `conversations/reconcile.rs` `release_unlisted` (test `reconcile_ends_unlisted_claimed_claims_and_holds_unlisted_running_ones_as_ending`) | the turn ends as "unknown"; the S4 marker rules then decide the give-back |

The clock is the holder's lease clock (`clock.rs`, counts sleep; tests inject `Fake`). Not modelled: the exact bound (the model
allows any time; the code's is a choice, longer than a core restart, minutes not hours).

## When you change covered code

1. `verification/holder-idle/drift.sh`. Nothing listed: no work. Listed files: read each diff.
2. Decide per change: no effect on the model, the model no longer matches (a new place that records an exit, a new way a core
   link ends, a new way an entry is dropped), or the rule changed on purpose. Update `WinKeeper.tla` and the cfgs from the
   code, not from what makes a result pass.
3. `verification/holder-idle/run.sh`. A row that differs is drift or a bug.
4. Only then set `SYNC` in `drift.sh` and the sync commit below.

**Sync commit:** `5664661d92d536d250bab8231655ddf9f0379948` (the fix).

## Map of the folder

- `WinKeeper.tla`, `HolderIdleGen.tla`: the models (`gen-*` rows use the second). `cfgs/*.cfg`: one complete cfg per row. `checks.tsv`: the table `run.sh` enforces.
  `run.sh`: pinned tools and the table. `drift.sh`: sync commit and covered files.
- The lease logic (`expire_lease`, `watch_lease`) is not covered here; see the holder-lease skill.
