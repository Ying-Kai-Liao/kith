---
name: holder-idle
description: Model-checks (TLA+/TLC) that a holder with an unacked exit record and no core connected ends instead of staying until reboot, and that dropping the record is safe. Use when a change touches core/holder/src/holder.rs (expire_exits, EXIT_KEEP, Config::exit_keep, watch_idle, idle_since, core_lost, drop_core, the ACK arm, reap, where an exit is recorded), core/holder/src/main.rs (the exit_keep plumbing), or the core's handling of a claim the holder no longer lists (core/runtime/src/holder_link.rs reconcile call, core/runtime/src/conversations/reconcile.rs release_unlisted). Also use to answer "does a holder stay forever after an uninstall?" and "can a dropped exit record free a claim under a live writer?".
---

# Holder idle: an unacked exit is dropped only while no core is connected

The holder keeps a process's exit record until a core acks it (`p::ACK`), and idles out only with an empty list and no core
(`watch_idle`). After an uninstall, or a stop followed by a run ending, a recorded exit was never acked and the holder never
left (until reboot). The fix (`Holder::expire_exits`): drop an exit record only while `st.core.is_none()`, and only once
`exit_keep` (default `EXIT_KEEP`, 10 minutes) has passed since the LATER of the exit and the link loss. Never while a core
is connected (a slow ack keeps its exit code). When the list is empty and no core, `idle_since` is set as the ACK arm does,
and `watch_idle` ends the holder after its usual idle time.

The model is `verification/holder-idle/WinKeeper.tla` (the W12 keeper model, with the `Expire`, `HolderIdleExit` and core-link
actions added 2026-11-27 by the architect; results in the architect's `model-w12/keeper/results.md`, section "Addendum
(2026-11-27, later)"). This skill is the only documentation here.

## Run it

```
verification/holder-idle/run.sh                 # all rows of checks.tsv (about a minute)
verification/holder-idle/run.sh --show CFG      # TLC's full output (the counterexample) for one row
verification/holder-idle/drift.sh               # which covered files changed since the sync commit
```

CI runs `run.sh` (job `holder-idle`) when `verification/holder-idle/`, this skill, `holder.rs` or `holder_link.rs` change.
Tools are the same pinned, sha256-checked `tla2tools.jar` v1.7.4 and Temurin JRE as the holder-lease skill (`run.sh` is a copy
of its harness); each row of `checks.tsv` names a complete cfg in `cfgs/`.

## Rows

| Cfg | Expected | What it shows |
|---|---|---|
| `idle-today-live` (no `Expire`, core gone for good) | fail, `HolderIdles` | the terminal ends, its exit is recorded, the core goes away: the record stays and the holder never idles out |
| `idle-fix-live` (`Expire`) | pass | the holder drops the record, then idles out |
| `idle-fix-safe-windows`, `idle-fix-safe-unix` (`Expire`, core down then back) | pass | `OneWriter`, `ExactlyOnce`, `NoEndWhileLive`, `AnsweredDelivered` on both OSes (what each does to a leftover child differs) |
| `idle-broken-RecordBeforeSweep-unix` | fail, `OneWriter` | the exit is recorded while a child of the program still runs; the core acks and frees the claim; a host turn starts: two writers. Fails without any expiry |

## Code to model

| Model | Code | Notes |
|---|---|---|
| exit record `texit`, written after the sweep | `holder.rs` `reap`: `proc.sweep()`, then the record (and `exited_at`) | **Keep the order.** It is what makes both the ack and the expiry safe. The comment at the sweep names `RecordBeforeSweep` |
| core link down | `drop_core` sets `core_lost` (the lease clock) | `core_lost` starts at the holder's start: a holder no core ever reached counts from there |
| `Expire` | `expire_exits`, run by `watch_idle` every 100 ms | only `held.exit.is_some()` entries; only `core.is_none()`; due at `max(exited_at, core_lost) + exit_keep`; zero `exit_keep` = never |
| `HolderIdleExit` | `watch_idle`: no core, `procs` empty, `idle_since` older than `idle` | `expire_exits` sets `idle_since` when it empties the list |
| core's reconcile of an unlisted claim | `conversations/reconcile.rs` `release_unlisted` (test `reconcile_ends_unlisted_claimed_claims_and_holds_unlisted_running_ones_as_ending`) | what is lost is the exit code and the `lapsed` flag: the turn ends as "unknown"; the S4 marker rules then decide the give-back |

The clock is the holder's lease clock (`clock.rs`, counts sleep; tests inject `Fake`). Not modelled: the exact bound (the model
allows any time; the code's is a choice, longer than a core restart, minutes not hours).

## When you change covered code

1. `verification/holder-idle/drift.sh`. Nothing listed: no work. Listed files: read each diff.
2. Decide per change: no effect on the model, the model no longer matches (a new place that records an exit, a new way a core
   link ends, a new way an entry is dropped), or the rule changed on purpose. Update `WinKeeper.tla` and the cfgs from the
   code, not from what makes a result pass.
3. `verification/holder-idle/run.sh`. A row that differs is drift or a bug.
4. Only then set `SYNC` in `drift.sh` and the sync commit below.

**Sync commit:** `PLACEHOLDER` (the fix).

## Map of the folder

- `WinKeeper.tla`: the model. `cfgs/*.cfg`: one complete cfg per row. `checks.tsv`: the table `run.sh` enforces.
  `run.sh`: pinned tools and the table. `drift.sh`: sync commit and covered files.
- The lease logic (`expire_lease`, `watch_lease`) is not covered here; see the holder-lease skill.
