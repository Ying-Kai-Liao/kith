---
name: schedule-fire
description: Model-checks (TLA+/TLC) the schedule fire protocol, the rule that a scheduled run fires at most once per slot, never stacks on the job's previous turn, never fires for a paused job or a revoked person, and expires at its deadline. Use when a change touches core/runtime/src/schedules/ (the M4-3 tick, fire, run now and webhook entry), kernel/bin/kith-sched (tick, the catch-up window, the paused and "still running" skips), the sched_tick or sched_fires tables, or the deadline, parked and expire handling of turns (enqueue_turn with source 'schedule', M3-2 claim and expiry). Also use to answer "can a schedule fire twice, stack up, or run for a revoked person?".
---

# Schedule fire: one transaction per fire, one turn per slot

The design (architect, 2026-11-04): a fire is ONE transaction: (1) insert into `sched_fires`, unique per
(agent, job, slot); (2) re-check `may` for the job's person; (3) skip if the job's previous turn has not finished;
(4) `enqueue_turn(source 'schedule', person, deadline = next slot)`. q218: a parked or queued scheduled turn expires
at its deadline. Run now uses slot `manual:<id>` (a webhook `event:<id>`) with the same skip rule. The tick's
catch-up runs once when the miss is under a window (a day).

The model is `verification/schedule-fire/Fire.tla`. This skill is the only documentation; there is no README.
**It was written by a stand-in (rust-m3-fire-model) for the architect's later review, before M4-3 existed.** Every
item under "Findings for M4-3" is the stand-in's proposal until the architect confirms it.

## Run it

```
verification/schedule-fire/run.sh                       # all rows of checks.tsv
verification/schedule-fire/run.sh --show VARIANT PROP   # TLC's full output (the counterexample) for one row
verification/schedule-fire/drift.sh                     # which covered files changed since SYNC
```

CI runs `run.sh` (job `schedule-fire`) on pull requests and pushes that touch `verification/schedule-fire/`, this
skill, or the covered code. `run.sh` exits 0 only when every row of `checks.tsv` (variant, kind, property, expected
pass/fail) came out as expected. Tools are the same pinned, sha256-checked `tla2tools.jar` v1.7.4 and Temurin JRE as
the holder-lease and writer-lock skills (`run.sh` is a copy of their harness). The design config has 2,871,168
states and about 80 s a row; a failing row stops at its first counterexample. A row that changes (a finding fixed, a
new break) exits 1: read this skill, do not edit the table to fit.

## What is modelled

Slots at times P, 2P .. NS*P (NS = 2, P = 2); the clock runs 0..6. One job with one person and a definition
(`paused`, `person`, `ver`: the index row), one edit, two persons who can be revoked and granted, 3 turn rows, one ad
hoc slot (run now and a webhook are the same code path). Actions: the tick (fresh clock, or a clock read before the
transaction: stale), an ad hoc fire, edit, revoke/grant, host crash and restart (a claim dies with the core), the
conversation held and released, time, and the turn lifecycle (claim or park, expire, start, finish).

Not modelled: cron arithmetic, a webhook's payload, today's 6 h "still running" window (a turn is unfinished until it
ends or expires), the delivery of output, claim fencing (M3-2's model), several jobs (independent: the key is (agent,
job, slot)), a second core.

Fairness: time, ticks, restart, claim, expire, start and finish are weakly fair, and the host crashes finitely often.
The last matters: without it TLC finds claim -> crash -> restart -> claim forever, a turn that never runs, which is a
crash loop (an operations problem), not a flaw in the fire protocol.

## Properties

| Property | Plain sentence | Harm |
|---|---|---|
| `TypeOK` | Values stay in range. | The model is wrong. |
| `AtMostOncePerSlot` | One turn per slot (including `manual:`/`event:` slots). | A job runs twice for one slot. |
| `NoStacking` | At most one unfinished turn per job. | Runs pile up behind a held conversation. |
| `CrashSafe` | A fire row and its turn exist together. | A crash loses or doubles a fire. |
| `NoRunForRevoked` | After a revoke commits, none of the person's turns is queued, parked, claimed or running. | A revoked person's job keeps running. |
| `CatchUpWindow` | A catch-up never runs a slot older than the window. | A day-old job fires on wake. |
| `CatchUpOnce` | One fire per missed span (no two fires from one clock reading). | A backlog burst. |
| `NoFireForRevokedPerson` | No turn is created for a revoked person. | A revoked person's job fires. |
| `PausedNeverFires` | No turn is created for a paused job. | A paused job fires. |
| `PausedNeverStarts` | After a pause commits, no turn is claimed. | A paused job still starts. |
| `EditUsesNewDefinition` | A new turn carries the current definition (person, ver). | An edit is ignored for one run. |
| `DeadlineExpires` | A turn is claimed only before its deadline (the claim is the start). | A late run (q218). |
| `SkipIsJustified` | A skip is recorded only when a live turn exists (not a parked turn past its deadline). | A slot lost to a dead turn. |
| `DueGetsRow` (liveness) | Once time is over the last slot has a row (fired or skipped with the reason). | A slot with no trace. |
| `TurnsEnd` (liveness) | A turn whose deadline is inside the model ends. | A turn stuck forever. |
| `Never*` | Each step happens (fire, skip kinds, catch-up, park, expire, done, cancel, crash): the invariant FAILS. | A property passing because nothing happens. |

## Findings for M4-3 (the stand-in's proposals)

a. The fire transaction re-reads the job's index row (paused, person, ver) and `may` inside the transaction (`stale-def`, `stale-may`).
b. Pause cancels the job's queued, parked and claimed turns; revoke cancels the person's unfinished turns (`pause-keeps`, `revoke-keeps`).
c. Deadline = the first slot after max(slot, the clock read), not after the slot, else a catch-up turn is expired on arrival.
d. The fire transaction sweeps turns past their deadline before the skip check (`no-sweep`: unjustified skips).
e. A skip keeps its `sched_fires` row with the reason (`no-record`).
f. Run now and webhooks obey the same skip rule (`manual-bypass`); run now on a paused job is skipped (stand-in choice).
g. The catch-up fires only the latest due slot, inside the window (`catch-each`, `no-window`).
h. A queued turn keeps the definition it was enqueued with; the commit of a pause or edit is the index row, not the `jobs.json` write.
i. The claim is the start: the deadline is checked at claim only (`no-deadline`).

## Code to model

Nothing of M4-3 exists yet. The table maps the model to the plan's names; when M4-3 lands the first reader checks it.

| Model action | Planned code | Today |
|---|---|---|
| `TickFresh`, `TickStale` | `schedules::tick`: read clock, read `sched_tick`, pick the slot, fire, write `sched_tick` | `kernel/bin/kith-sched` `tick()` |
| `FireOne` | `schedules::fire`: the one transaction | the skip rules in `tick()` |
| `AdHocFire` | `schedules::fire` for `manual:<id>` / `event:<id>` | run now in kith-sched |
| `Edit`, `Revoke`, `Grant` | the core re-indexing `jobs.json`; authority tables (M2) | |
| `Claim`, `Expire`, `Start`, `Finish`, `Crash`, `Restart`, `Hold`, `Release` | M3-2 turn rules (claim/park, deadline, expire) | |

## Broken variants

| Variant | Switch | Fails |
|---|---|---|
| `no-unique` | `Unique = FALSE` | `AtMostOncePerSlot` |
| `insert-first`, `enqueue-first` | the transaction split in two steps | `CrashSafe` (and `AtMostOncePerSlot` for `enqueue-first`) |
| `no-skip` | `SkipPrev = FALSE` | `NoStacking` |
| `manual-bypass` | `ManualSkips = FALSE` | `NoStacking` |
| `no-sweep` | `SweepInFire = FALSE` | `SkipIsJustified` |
| `stale-may` | `MayInTxn = FALSE` | `NoFireForRevokedPerson` |
| `stale-def` | `DefInTxn = FALSE` | `PausedNeverFires`, `EditUsesNewDefinition` |
| `pause-keeps` | `PauseCancels = FALSE` | `PausedNeverStarts` |
| `revoke-keeps` | `RevokeCancels = FALSE` | `NoRunForRevoked` |
| `no-deadline` | `DeadlineCheck = FALSE` | `DeadlineExpires` |
| `no-window` | `Window = FALSE` | `CatchUpWindow` |
| `catch-each` | `CatchMode = "each"` | `CatchUpOnce` |
| `no-record` | `RecordSkips = FALSE` | `DueGetsRow` |
| `wide-window`, `live` | a wider window; time guarded | pass (`CatchUpOnce`; liveness) |

## When you change covered code

1. `verification/schedule-fire/drift.sh`. Nothing listed: no work. Listed files: read each diff and commit message.
2. Decide per change: no effect on the model; the model no longer matches (a step split, a check moved, a new
   entry path); or the rule changed on purpose. Update `Fire.tla` and `base.consts` from the code, not from what makes
   a result pass. One step in code = one action; cite the function above the action.
3. `verification/schedule-fire/run.sh`. A row that differs is drift (replay the TLC trace against the code) or a bug.
4. A pass can have lost the ability to fail: break the new check in a scratch edit and see `run.sh` fail.
5. Only then set `SYNC` in `drift.sh` and the sync commit below.

**Sync commit:** `2f102cab4cdd352a651e041ff3fec61ef593601a` (the base the model was written on; M4-3 does not exist
yet). When M4-3 merges, the first reader checks the code against the table above and replaces this with the merge commit.

## Map of the folder

- `Fire.tla`: the model. `base.consts`: the switches for the design. `variants/*.set`: overrides per variant.
- `checks.tsv`: the table `run.sh` enforces. `run.sh`: pinned tools and the table. `drift.sh`: sync commit and covered files.
