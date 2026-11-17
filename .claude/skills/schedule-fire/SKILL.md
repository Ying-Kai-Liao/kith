---
name: schedule-fire
description: Model-checks (TLA+/TLC) the schedule fire protocol, the rule that a scheduled run fires at most once per slot, never stacks on the job's previous turn, never fires for a paused job or a revoked person, expires at its deadline, leaves a row for every slot, and tells a run-now or webhook caller busy without taking its key. Use when a change touches core/runtime/src/schedules/ (the M4-3 tick, fire, run now and webhook entry), kernel/bin/kith-sched (tick, the catch-up window, the paused and "still running" skips), the sched_tick or sched_fires tables, or the deadline, parked and expire handling of turns (enqueue_turn with source 'schedule', M3-2 claim and expiry). Also use to answer "can a schedule fire twice, stack up, run for a revoked person, or lose a slot or a webhook?".
---

# Schedule fire: one transaction per fire, one turn per slot, a row for every slot

The design (architect, 2026-11-04, reviewed on PR #751): a fire is ONE transaction: (1) insert into `sched_fires`,
unique per (agent, job, slot) with no `mac`; (2) re-check `may` for the job's person; (3) skip if the job's previous
turn has not finished; (4) `enqueue_turn(source 'schedule', person, deadline)`. q218: a parked or queued scheduled turn
expires at its deadline. Run now uses slot `manual:<id>` (a webhook `event:<id>`) with the same skip rule, but a BUSY
one writes no key and tells its caller. The tick's catch-up runs once, for the latest due slot, when the miss is under
a window (a day); every slot the watermark passes without a fire gets a `skipped:missed` row.

The model is `verification/schedule-fire/Fire.tla`. This skill is the only documentation; there is no README.
It was written before M4-3 existed, by a stand-in, and reviewed by the architect (PR #751 comment): the "M4-3
requirements" below are his list, each naming the row that fails without it.

## Run it

```
verification/schedule-fire/run.sh                       # all rows of checks.tsv
verification/schedule-fire/run.sh --show VARIANT PROP   # TLC's full output (the counterexample) for one row
verification/schedule-fire/drift.sh                     # which covered files changed since SYNC
```

CI runs `run.sh` (job `schedule-fire`) on pull requests and pushes that touch `verification/schedule-fire/`, this
skill, or the covered code; by hand, only this job: `gh workflow run ci.yml --repo Ying-Kai-Liao/kith --ref <branch> -f mode=model`.
`run.sh` exits 0 only when every row of `checks.tsv` (variant, kind, property, expected pass/fail) came out as
expected. Tools are the same pinned, sha256-checked `tla2tools.jar` v1.7.4 and Temurin JRE as the holder-lease and
writer-lock skills (`run.sh` is a copy of their harness). `fixed` (the design) has 283,124 states, `rev` (the design
with one revoke per person and no edit) 3,767,524; a row takes from a few seconds to about two minutes, the whole table (80 rows) about 30 minutes on CI (four workers)
(the CI schedule-fire job runs the whole table, not --smoke, when any covered path or this folder changes). A failing row stops at its first counterexample. A row that changes (a finding fixed, a new break)
exits 1: read this skill, do not edit the table to fit.

## What is modelled

Slots at times P, 2P .. NS*P (NS = 2, P = 2); the clock runs 0..6. One job with one person and a definition
(`paused`, `person`, `ver`: the index row), three turn rows (a claim happens only for an owner turn that has a bot: person 1
has one, person 2 has none, so person 2's turns only ever expire; `HostBotOnly`), one ad hoc slot (run now and a webhook are the same code
path), and the caller's answer for it (`ans`: none, busy, or the recorded outcome). Actions: the tick (fresh clock, or
a clock read before the transaction: stale), an ad hoc fire or its retry, edit (with a pause cancel), the authority
revoke, then `revoke_person` in its own transaction, grant, the core's crash and restart (rule K: the host's live claims
stay, so claimed and running turns are kept), the host's own restart (`HostAdopt`, today's adopt: a claim never redeemed
ends and its turn is queued again; a running one is killed and its turn is queued again), the
conversation held and released, time (it runs while the core is down), and the turn lifecycle (claim with its may
check, or park; expire; start; finish).

Two sizes keep the state space small: `fixed` has one edit and no revoke; `rev` has one revoke per person and no edit
(the person of the job never changes there). An edit that moves the job to a revoked person is not checked.

Not modelled: which process (the core's reconcile, the host's adopt, a holder) ends a claim, a claim's pid and the redeem
(a `running` turn is one state), `Crash` and `HostAdopt` at every point of a transaction (a transaction is atomic),
cron arithmetic, a webhook's payload, today's 6 h "still running" window (a turn is unfinished until it
ends or expires), the delivery of output, claim fencing (M3-2's model), several jobs (independent: the key is (agent,
job, slot)), a second core, the run-now / webhook TTL (their deadline is the next slot after the clock only), the
`jobs.json` hash compare (requirement 3), the watermark as a UTC instant in ms and never moving back (requirement 10).

Fairness: time, restart and the second half of a split fire are weakly fair; ticks, claim, expire, start and finish are
strongly fair, and a retried webhook delivery is strongly fair in the `hook` row. The core may crash any number of
times: without strong fairness TLC finds claim -> crash -> restart -> claim forever, a turn that never runs, which is a
crash loop (an operations problem). Strong fairness lets a turn run in the stretches where the core is up. (An earlier
version assumed finitely many crashes; the review showed that hid slots missed while the core is down.)

## Properties

| Property | Plain sentence | Harm |
|---|---|---|
| `TypeOK` | Values stay in range. | The model is wrong. |
| `AtMostOncePerSlot` | One turn per slot (including `manual:`/`event:` slots and a retry of them). | A job runs twice for one slot. |
| `NoStacking` | At most one unfinished turn per job. | Runs pile up behind a held conversation. |
| `CrashSafe` | A fire row and its turn exist together. | A crash loses or doubles a fire. |
| `NoRunForRevoked` | After `revoke_person` has run, none of the person's turns is queued, parked, claimed or running. | A revoked person's job keeps running. |
| `CatchUpWindow` | A catch-up never runs a slot older than the window. | A day-old job fires on wake. |
| `CatchUpOnce` | One fire per missed span (no two fires from one clock reading). | A backlog burst. |
| `WatermarkRows` | Every slot at or before the watermark has a row (fired, skipped with the reason, or `skip_missed`). | A slot with no trace. |
| `AnswerIsRecorded` | A run-now / webhook caller's final answer is the recorded outcome. | The caller is told something the table does not say. |
| `NoFireForRevokedPerson` | No turn is created for a revoked person once `revoke_person` ran (one created before it is cancelled by it). | A revoked person's job fires. |
| `PausedNeverFires` | No turn is created for a paused job. | A paused job fires. |
| `PausedNeverStarts` | After a pause commits, no turn is claimed. | A paused job still starts. |
| `RevokedNeverStarts` | After `revoke_person` has run for a revoke, no turn of the person is claimed. (Between the authority revoke and `revoke_person` a claim with a stale may check can still commit; `revoke_person` then cancels it.) | A revoked person starts a run after `revoke_person`. |
| `EditUsesNewDefinition` | A new turn carries the current definition (person, ver). | An edit is ignored for one run. |
| `DeadlineExpires` | A turn is claimed only before its deadline (the claim is the start). | A late run (q218). |
| `FiredIsLive` | A new turn's deadline is after the clock its fire read. | A run-now or catch-up turn dead on arrival. |
| `SkipIsJustified` | A skip is recorded only when a live turn exists (not a parked turn past its deadline). | A slot lost to a dead turn. |
| `BusyWritesNoKey` | A busy answer to a run-now / webhook writes no `sched_fires` row. | Every retry is a silent no-op for ever. |
| `DueGetsRow` (liveness) | Once time is over the last slot has a row, with the core crashing and time running while it is down. | A slot missed while the core was down vanishes. |
| `TurnsEnd` (liveness) | A turn whose deadline is inside the model ends, with any number of crashes. | A turn stuck forever. |
| `HookRuns` (liveness) | A webhook delivery retried until accepted gets its turn (no revoke, no pause, no hold). | A busy webhook is never run. |
| `Never*` | Each step happens (fire, skip kinds incl. missed, busy, catch-up, park, expire, done, cancel, crash): the invariant FAILS. | A property passing because nothing happens. |

## M4-3 requirements (the architect's list; the row that fails without each)

1. **One write transaction per fire** (`BEGIN IMMEDIATE` via `claim_tx`). Inside it, in order: read the watermark, the
   job's index row and the revoke epoch; insert the `sched_fires` row (agent, job, slot); sweep the job's queued/parked
   turns past their deadline; apply the skip rule; enqueue the turn; write `turn_id` into the fire row; move the
   watermark. `enqueue_turn` and `expire_parked` each open and commit their own transaction today (`claims.rs:958`,
   `:1067`), so calling them in sequence is the split shape: add forms that take `&Transaction`. Only the clock may be
   read before the transaction. Rows: `insert-first`, `enqueue-first` (`CrashSafe`), `no-unique` (`AtMostOncePerSlot`),
   `no-sweep` (`SkipIsJustified`), `tick-first` (`WatermarkRows`; `tick-first-live` `DueGetsRow`: the watermark saved
   before the fire loses the slot on a crash in between).
2. **May check:** read `revoke_epoch(person)`, check may, and refuse in the transaction as `skipped:revoked` if the
   epoch moved (`NewClaim::revoke_seen`, checked at `claims.rs:273`). "May inside the transaction" means exactly this unless the
   authority is a core.db table read in the same transaction. Rows: `stale-may` (`NoFireForRevokedPerson`; `rev` and
   `may-in-txn` pass).
3. **Definition:** the index row decides; before deciding, compare the `jobs.json` hash (read under
   `<agent>.jobs.lock`) with the index's and re-index if it changed. Rows: `stale-def` (`PausedNeverFires`,
   `EditUsesNewDefinition`). The hash compare itself is not modelled.
4. **Skip rule:** "previous turn unfinished" means any unfinished turn of the job, found through `sched_fires.turn_id`
   across all its slots, including `manual:` and `event:` (`turns` has no job column). Rows: `no-skip`,
   `manual-bypass` (`NoStacking`).
5. **Pause and revoke cancel:** every index write with `paused = true` cancels the job's queued and parked turns, on
   every write, in the same transaction as the index write. A claimed turn with no pid gets its claim ended with the
   check-and-set `revoke_person` uses; a running turn is left alone. Revoke is `revoke_person` (`claims.rs:673`), which
   runs after the authority revoke is visible. What holds is `revoke_person` cancelling the person's pending turns in the same transaction as the epoch bump; the claim's may re-check (`revoke_seen`) is a second guard for scheduled turns, not the thing that holds. Rows: `pause-keeps` (`PausedNeverStarts`),
   `pause-later` (cancel in a second transaction: `PausedNeverStarts`), `revoke-keeps` (`NoRunForRevoked`),
   `claim-no-may` (passes: `RevokedNeverStarts` holds without the claim's may check).
6. **Deadline:** a timed slot gets its next slot; a run-now or webhook fire gets the earlier of the job's next slot
   after now and now + TTL (a job with no timed trigger gets the TTL); the deadline is checked at the claim only
   (`claims.rs:350`). The model has no TTL: its ad hoc deadline is the next slot after the clock. Rows: `dl-from-slot`
   (`FiredIsLive`: a run-now at time 2 gets deadline 2), `no-deadline` (`DeadlineExpires`).
7. **Rows:** every timed decision keeps a row with its reason, and a slot missed past the window (or passed by a
   later catch-up) gets `skipped:missed`, per slot or as one span row. Rows: `no-record` (`DueGetsRow`), `no-missed`
   (`WatermarkRows`), `no-missed-live` (`DueGetsRow`: the core is down from 0 to 6 and neither slot gets a row).
8. **Run now and webhooks:** same skip rule. Busy writes no key and answers busy (run-now) or retry-later (webhook); a
   decided id answers its recorded outcome on retry; a paused or revoked job writes the row and answers final. Rows:
   `busy-row` (`BusyWritesNoKey`), `hook-busy-row` (`HookRuns`: the retry finds the skip row for ever; `hook` passes),
   `no-unique` (a retry that fires again).
9. **Catch-up:** only the latest due slot inside the window (under a day). Rows: `catch-each` (`CatchUpOnce`),
   `no-window` (`CatchUpWindow`).
10. **Watermark:** a UTC instant in ms; m0120's `sched_tick.tick` holds local-minute text, which repeats when the
    clocks go back: convert at import. Never move it backwards. Not modelled (the model's watermark only grows).
11. **Before M4-5:** nothing wires the core's fire to a live tick while kith-sched owns `jobs.json`. Not a model
    matter.

## Follow-ups (not in the model)

- Two schedulers, two key spaces: until M4-5 kith-sched's `state.json` and `sched_fires` share no key, so the core must
  not fire from a live tick.
- At the M4-5 switch: stop kith-sched's tick in the same step and import its last-fired slots into `sched_fires`, else
  the first core tick catches up a slot kith-sched already ran.
- Mac moves: moving a job between Macs through a synced `jobs.json` can fire one slot on both Macs; accept that or make
  the move take effect from the next slot.

## Code to model

M4-3 is `core/runtime/src/schedules/fire.rs` (with `sync.rs`, the index). The table maps each model action to the code
(`claims.rs` lines are as of the M3-2 API).

| Model action | Code |
|---|---|
| `TickFresh`, `TickStale` | `schedules::fire::tick`: reads the clock and the watermark, settles each agent's index, plans each job (`plan`: the latest due slot in the window fires, the rest are one `missed` span), fires, moves the watermark in the last fire's transaction. A clock read before the transaction is the stale case |
| `FireOne` | `fire::fire` / `fire_in`: one `claim_tx` over `sched_fires` (unique per agent, job, slot) and the turn, steps 1-7 in the module header |
| `AdHocFire` | `fire::run_now` and the webhook entry, `manual:<id>` / `event:<id>` slots, the answer to the caller (`Outcome::Busy` writes no key) |
| `Edit`, `CancelPaused` | `sync::sync_in` (re-index on a hash change, `cancel_in` on every write with `paused`), `index` / `read_jobs` in `schedules/mod.rs` |
| `RevokeAuth`, `Grant` | authority tables (M2) |
| `RevokePerson` | `revoke_person` (`claims.rs:673`): cancels queued and parked turns, ends live claims, bumps the epoch |
| `Claim` (may check) | `claim` (`:246`), `revoke_epoch` (`:182`), `NewClaim::revoke_seen` (`:91`, checked `:273`), the deadline check in the claim's UPDATE (`:350`) |
| `Hold`, `Release`, park | `park_in` (called by `fire_in` when the conversation is held), `unpark_ready` |
| `Expire` | `expire_turns_in` (the sweep in `fire_in`, step 3), `expire_parked` (q218) |
| `Start`, `Finish` | `redeem` (`:373`), `report` (`:415`) |
| `Crash`, `Restart` | the core restarting: `reconcile` (`conversations/reconcile.rs`: step 5 keeps the host's live claims, rule K of design 2026-11-16-core-queued-runner). Turns are left as they are |
| `HostAdopt` | the host's own restart: `hostclaim.adopt` (`hostclaim.rs:219`) -> `conversations/warm.rs` `adopt` (`:247`): a `claimed` claim with no pid ends (`host_changed`) and its turn is queued again; one with a pid goes to `ending`, its turn is queued again and the process is killed. Package C of M4-5 changes this for scheduled runs (see "Pending code") |
| `Claim` scoped to a bot | the host's runner (`routines/core_runs.rs`) begins only turns of an owner that has a bot; a turn it cannot run is left to expire |
| the turn row | `enqueue_turn_in` (`fire_in`, step 5), source `schedule` |
| who runs the turn (not in `Fire.tla`; `HostRuns.tla` in the architect's design folder) | the host's runner: `hostclaim.queued` (`hostclaim.rs`, unkeyed `schedule` turns, `redeemedBefore`), `serve/host/src/routines/core_runs.rs` (pull after each `schedule.tick` and every 2 s in Writer mode; a turn redeemed before is cancelled "interrupted", any other begins by its id through the bot's actor, never enqueued). `RUNS_FIRED_TURNS` is off again since #1035 (architect MUST 1) and comes back on only in the PR that lands the temp-home e2e (package F) |

## Pending code: the handover (M4-5, packages A2, B, C; the model is `Handover.tla`)

`Handover.tla` models kith-sched, the host's routines (J2) and the core during the switch: the operator's step (pre,
import, tombstone, switched), the core's tick, and the rollback. Properties: `AtMostOnce` (no slot is covered by two
runs), `NoSilentLoss` (every slot a scheduler evaluated is covered or has a decided row) and `NoStack` (no job has two
running runs). Design and verdict: the architect's `2026-11-17-schedules-handover` folder (design.md, model/results.md).
The rules, each with the `h-*` row that fails without it:

| Rule | What | Code | Row |
|---|---|---|---|
| H1 `tick.lock` | kith-sched's tick and the step share one lock; the tick checks the tombstone under it | pending: packages A1 (`kith-sched`), A2 (the step) | `h-no-lock` (`AtMostOnce`) |
| H2 anchor | J2 (a host routine) counts from its imported `next_run` (the anchor) until its first core plan, never from the watermark; the once row is the anchor too | pending: package A2 (import) and B (plan) | `h-no-anchor` (`AtMostOnce`, `NoSilentLoss`), `h-wm-back` (`AtMostOnce`) |
| H3 live old run | an imported kith-sched run that is still live counts as unfinished for the core's skip rule | pending: package A2 (import writes `last` into `sched_runs`) and B | `h-no-h3` (`NoStack`) |
| H4 per-job rollback | a rollback rewinds only the job it concerns: its next run counts from where it would next count (the anchor until used, never below anchor - 1), the core's cancelled runs leave a `skipped:rollback` row, a running core run is written into `state.json`'s `last` last | pending: package C (rollback) | `h-perjob-as-written` (`AtMostOnce`), `h-global-rewind` (`AtMostOnce`), `h-bare` (`NoSilentLoss`), `h-nolast` (`NoStack`) |

Package C's change of the host's adopt for scheduled runs (pid alive: keeps running; redeemed then dead: ends and is
never retried; no pid: queued again) is not in `Fire.tla` (it models today's adopt) and not in `Handover.tla` (it has no
host restart mid-run). Re-check both when C lands.

`Handover.tla` does not model: the 1-day catch-up windows, interval and once triggers (H2's once row and the interval
anchor are argued, not checked), webhooks, a host restart mid-run (C), two Macs, a crash inside kith-sched's locked
section. Bounds: `MaxMin` 3 (4 did not finish in 20 minutes on a busy Mac), J1 a kith-sched cron job due every minute,
J2 a host routine due every minute. The architect's first H4 fix (`next_run = max(wm, anchor-1) + 1`) failed
`NoSilentLoss` too (about 86k states); it has no row here, `perjobfrom` is the fix. State counts of the rows are in
`results` below.

## Broken variants

| Variant | Switch | Fails |
|---|---|---|
| `no-unique` | `Unique = FALSE` | `AtMostOncePerSlot` |
| `insert-first`, `enqueue-first` | the transaction split in two steps | `CrashSafe` (and `AtMostOncePerSlot` for `enqueue-first`) |
| `tick-first` | `TickFirst = TRUE` | `WatermarkRows`; `tick-first-live`: `DueGetsRow` |
| `no-skip` | `SkipPrev = FALSE` | `NoStacking` |
| `manual-bypass` | `ManualSkips = FALSE` | `NoStacking` |
| `no-sweep` | `SweepInFire = FALSE` | `SkipIsJustified` |
| `busy-row`, `hook-busy-row` | `BusyTakesNoKey = FALSE` | `BusyWritesNoKey`; `HookRuns` |
| `stale-may` | `EpochCheck = FALSE` (on `rev`) | `NoFireForRevokedPerson` |
| `stale-def` | `DefInTxn = FALSE` | `PausedNeverFires`, `EditUsesNewDefinition` |
| `pause-keeps` | `PauseCancels = FALSE` | `PausedNeverStarts` |
| `pause-later` | `PauseLater = TRUE` | `PausedNeverStarts` |
| `revoke-keeps` | `RevokeCancels = FALSE` (on `rev`) | `NoRunForRevoked` |
| `claim-no-may` | `ClaimMay = FALSE` (on `rev`) | pass: `RevokedNeverStarts` holds without the claim's may check |
| `no-deadline` | `DeadlineCheck = FALSE` | `DeadlineExpires` |
| `dl-from-slot` | `DlFromSlot = TRUE` | `FiredIsLive` |
| `no-window` | `Window = FALSE` | `CatchUpWindow` |
| `catch-each` | `CatchMode = "each"` | `CatchUpOnce` |
| `no-missed`, `no-missed-live` | `MissedRows = FALSE` | `WatermarkRows`; `DueGetsRow` |
| `no-record` | `RecordSkips = FALSE` (live) | `DueGetsRow` |
| `wide-window`, `may-in-txn`, `rev`, `edit-rev`, `live`, `hook` | a wider window; may read in the same database; the revoke size; an edit and a revoke together (NS 1, no Hold); time guarded; redelivery | pass |
| `h-design` | `Handover.tla` as designed (H1 to H4) | pass: `AtMostOnce`, `NoSilentLoss`, `NoStack` |
| `h-no-lock`, `h-no-anchor`, `h-wm-back`, `h-no-h3`, `h-perjob-as-written`, `h-global-rewind`, `h-bare`, `h-nolast` | `TickLock = FALSE`; `Anchor = "wm"`; `Anchor = "wmback"`; `LiveSkip = FALSE`; `Rollback = "perjob"`, `"global"`, `"bare"`, `"perjobnolast"` | `AtMostOnce`; `AtMostOnce` and `NoSilentLoss`; `AtMostOnce`; `NoStack`; `AtMostOnce`; `AtMostOnce`; `NoSilentLoss`; `NoStack` |

The two "cancel in a second transaction" variants, run against every property (not all are in `checks.tsv`):
- `pause-later` (the pause commits, its cancel is a second transaction): only `PausedNeverStarts` fails (52 states: the
  turn is claimed while the job is paused). All 17 other properties pass.
- `claim-no-may` (the authority revoke and `revoke_person` are separate transactions, as in code; the claim's may check
  is the caller's, taken before the transaction against any past authority snapshot, and refused only when the revoke
  epoch moved, `claims.rs:32-37`): passes. A claim whose may check ran before the authority revoke still commits until
  `revoke_person` bumps the epoch; what stops a late claim is the turn's state, because `revoke_person` cancels the
  person's pending turns in the epoch's transaction. So `RevokedNeverStarts` is stated "after `revoke_person`", like
  `NoRunForRevoked` and `NoFireForRevokedPerson`. (An earlier version of the model read the live revoke in the claim and
  so held a stronger property than the code gives; the architect's scratch copy with the claim done the code's way
  failed it in 4 steps.)
- `edit-rev` (NS 1, one edit, one revoke per person, MaxTurns 2, no Hold): `NoRunForRevoked` and `NoFireForRevokedPerson`
  pass (1,184,126 distinct states, under a minute). The full-size shape did not finish in 20 minutes, hence the smaller one;
  it covers an edit that moves the job to a person being revoked.

## When you change covered code

1. `verification/schedule-fire/drift.sh`. Nothing listed: no work. Listed files: read each diff and commit message.
2. Decide per change: no effect on the model; the model no longer matches (a step split, a check moved, a new
   entry path); or the rule changed on purpose. Update `Fire.tla` and `base.consts` from the code, not from what makes
   a result pass. One step in code = one action; cite the function above the action.
3. `verification/schedule-fire/run.sh`. A row that differs is drift (replay the TLC trace against the code) or a bug.
4. A pass can have lost the ability to fail: break the new check in a scratch edit and see `run.sh` fail.
5. Only then set `SYNC` in `drift.sh` and the sync commit below.

**M4-5a notes (not modelled).**
- *Pruning vs `WatermarkRows`:* `sched_fires` rows older than 14 days (deliveries `event:d...`: 60) are deleted hourly, except a job's latest timed slot (an interval counts from it) and rows whose turn has not finished. `WatermarkRows` ("a row for every slot the watermark passed") is about slots between the old and the new watermark in one tick, never about slots weeks behind it: `plan` only looks forward from the watermark, so a pruned slot is never planned again. The limit is that a `manual:<key>` run-now key or a delivery id reused after its window fires again.
- *All-fail limit (10 ticks):* when nothing went through a tick with more than one job due, the failure may be the core's (a locked keychain, an unwritable database), so jobs get 10 failing minutes before each is given up on, against 3 when something else went through. It is a bounded hold, not tied to the 1-day catch-up window: that would hold the watermark (and make every tick plan from further back) for a day. Ten minutes covers a keychain prompt or a restart; longer outages are then recorded as `skipped:error` rows rather than silently replayed.
- *`mac` on a job:* a job naming another Mac is not planned, counted or recorded here. A webhook/event delivery (`not-here`, answered before the key is looked at) and run-now (refused) for such a job are refused too, with nothing written (M4-5b); this Mac's id is only read for the refusal, never made. A job with no `mac` is every Mac's.

- *Not modelled (adopt):* core reconcile step 5's requeue of a claim whose process is gone (`HostAdopt`'s any-time requeue over-approximates it). The out-of-usage keep path re-begins a running turn by the same id inside the actor, so a paused job's running turn can continue on the backup account; deliberate per rule 5.

**Sync commit:** `6f02eea582c60cf7c6a223d3254fcb7d5786e84b` (origin/rust-rebuild; Gap 10 re-sync: `Fire.tla` split Crash and Restart by rule K, added `HostAdopt` (today's adopt), the bot-only claim (`HostBotOnly`) with the runner's cancel of a turn redeemed before (`rd`, `RunnerCancel`; this is why `fixed PausedNeverStarts` passes: a pause leaves a running turn alone and adopt requeues it, but the runner then cancels it), and package C ahead of its brief (`AdoptMode` "c", `ClaimPausedCheck`; rows `c-nocheck`, `c-check`, `c-check-rev`); copied the architect's `Handover.tla` with its 12 `h-*` rows; `rev` now runs with `HostBotOnly = FALSE` so `RevokedNeverStarts` checks the core's claim rule for any claimer. `run.sh` was run in full and every row is as expected. The H1-H4 rows are pending packages A2/B/C; re-check them, C's adopt change and the HostAdopt / bot-only rows at the final re-sync.)

## Map of the folder

- `Fire.tla`: the model. `base.consts`: the switches for the design. `variants/*.set`: overrides per variant.
- `Handover.tla`: the M4-5 handover model (variants `h-*`, switches in `base.handover.consts`), copied from the architect's folder.
- `checks.tsv`: the table `run.sh` enforces. `run.sh`: pinned tools and the table. `drift.sh`: sync commit and covered files.
