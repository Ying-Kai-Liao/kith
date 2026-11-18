---
name: schedule-fire
description: Model-checks (TLA+/TLC) the schedule fire protocol, the rule that a scheduled run fires at most once per slot, never stacks on the job's previous turn, never fires for a paused job or a revoked person, expires at its deadline, leaves a row for every slot, and tells a run-now or webhook caller busy without taking its key. Use when a change touches core/runtime/src/schedules/ (the M4-3 tick, fire, run now and webhook entry), kernel/bin/kith-sched (tick, the catch-up window, the paused and "still running" skips), the sched_tick or sched_fires tables, or the deadline, parked and expire handling of turns (enqueue_turn with source 'schedule', M3-2 claim and expiry), who runs a fired turn (serve/host/src/routines/core_runs.rs and core_cron.rs, core/runtime/src/conversations/sched_run.rs claim_run and claim.attach, kith-sched run-turn, warm.rs adopt, reconcile.rs step 5, end_gone), or the M4-5 switch (core/runtime/src/cutover/modules/schedules*.rs: the step, the gate conditions, the rollback's hand-back; kith-sched's tick.lock). Also use to answer "can a schedule fire twice, stack up, run for a revoked person, or lose a slot or a webhook?", "can a host or core restart run a scheduled turn twice?" and "can an old kith-sched tick run a slot the core runs too?".
---

# Schedule fire: one transaction per fire, one turn per slot, a row for every slot

The design (architect, 2026-11-04, reviewed on PR #751): a fire is ONE transaction: (1) insert into `sched_fires`,
unique per (agent, job, slot) with no `mac`; (2) re-check `may` for the job's person; (3) skip if the job's previous
turn has not finished; (4) `enqueue_turn(source 'schedule', person, deadline)`. q218: a parked or queued scheduled turn
expires at its deadline. Run now uses slot `manual:<id>` (a webhook `event:<id>`) with the same skip rule, but a BUSY
one writes no key and tells its caller. The tick's catch-up runs once, for the latest due slot, when the miss is under
a window (a day); every slot the watermark passes without a fire gets a `skipped:missed` row.

There are three models in `verification/schedule-fire/`: `Fire.tla` (the fire, below), `Handover.tla` (the M4-5 switch from
kith-sched and the host's routines to the core, variants `h-*`) and `HostRuns.tla` (who runs a turn the core fired, across
host and core restarts, variants `hr-*`). This skill is the only documentation; there is no README.
It was written before M4-3 existed, by a stand-in, and reviewed by the architect (PR #751 comment): the "M4-3
requirements" below are his list, each naming the row that fails without it.

## Run it

```
verification/schedule-fire/run.sh                       # all rows of checks.tsv
VERIFY_JOBS=1 nice verification/schedule-fire/run.sh    # the same, one TLC at a time (a busy Mac; run.sh runs four at once)
verification/schedule-fire/run.sh --show VARIANT PROP   # TLC's full output (the counterexample) for one row
verification/schedule-fire/drift.sh                     # which covered files changed since SYNC
```

CI runs `run.sh` (job `schedule-fire`) on pull requests and pushes that touch `verification/schedule-fire/`, this
skill, or the covered code; by hand, only this job: `gh workflow run ci.yml --repo Ying-Kai-Liao/kith --ref <branch> -f mode=model`.
`run.sh` exits 0 only when every row of `checks.tsv` (variant, kind, property, expected pass/fail) came out as
expected. Tools are the same pinned, sha256-checked `tla2tools.jar` v1.7.4 and Temurin JRE as the holder-lease and
writer-lock skills (`run.sh` is a copy of their harness). `fixed` (the design) has 639,988 distinct states, `rev` (the design
with one revoke per person and no edit) 6,364,984, `h-design` 341,602, `h-old` 939,514, `hr-design` 18,414; a row takes from a
second to a few minutes, the whole table (106 rows) about 35 minutes on a Mac one row at a time, about 30 minutes on CI (four workers)
(the CI schedule-fire job runs the whole table, not --smoke, when any covered path or this folder changes). A failing row stops at its first counterexample. A row that changes (a finding fixed, a new break)
exits 1: read this skill, do not edit the table to fit.

## What is modelled

Slots at times P, 2P .. NS*P (NS = 2, P = 2); the clock runs 0..6. One job with one person and a definition
(`paused`, `person`, `ver`: the index row), three turn rows, one ad hoc slot (run now and a webhook are the same code
path), and the caller's answer for it (`ans`: none, busy, or the recorded outcome). The job is a kith-sched (cron) job
(`Cron`, the base since the M4-5 re-sync): the host's runner claims any person's turn and `kith-sched run-turn` runs it.
Variant `routine` is a host routine's job instead: only an owner turn that has a bot is claimed (person 1 has one, person
2 has none: the runner cancels person 2's turns, `RunnerGiveUp`), and the out-of-usage keep path exists (`KeepRebegin`).
Either way the runner never claims a turn redeemed before; it cancels it (`HostBotOnly`, `RunnerCancel`). Actions: the tick (fresh clock, or
a clock read before the transaction: stale), an ad hoc fire or its retry, edit (with a pause cancel), the authority
revoke, then `revoke_person` in its own transaction, grant, the core's crash and restart (rule K: the host's live claims
stay, so claimed and running turns are kept), the host's own restart (`HostAdopt`: a claim never redeemed ends and its
turn is queued again, or cancelled when the job is paused; a routine's running turn is killed and queued again, and the
runner then cancels it; a cron job's attached run keeps running, or ends when its process is gone, never requeued), the
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
| `HostAdopt` | the host's own restart: `hostclaim.adopt` (`hostclaim.rs:219`) -> `conversations/warm.rs` `adopt` (`:247`): a `claimed` claim with no pid ends (`host_changed`) and its turn is queued again; one with a pid goes to `ending`, its turn is queued again and the process is killed. Package C of M4-5 changed it for a cron job's run (`AdoptMode "c"`): its claim is kind `turn` with no process until the run attaches itself (`claim.attach`, `conversations/sched_run.rs`: kind `external`, `running`, pid + start); adopt requeues a claim with no pid (the same code as today), leaves an attached one alone, and `end_gone` ends one whose process is gone with its turn `failed`, never requeued. `requeue_turn` cancels a paused job's turn instead |
| `Claim` scoped to a bot | the host's runner (`routines/core_runs.rs`) begins a routine's turn through its owner's bot; a cron job's turn (any person, bot or none) is claimed (`sched_run::claim_run`) and run by `kith-sched run-turn` (`routines/core_cron.rs`); a turn the host cannot place is cancelled with a reason and a notice |
| `Claim` paused check (`ClaimPausedCheck`) | `claims::fence_in` -> `held_in` -> `sched_run::paused_in` (`Fence::Paused`, scoped to `source = 'schedule'`): a claim of a paused job's turn cancels it; `cancel_fenced` writes the cancel |
| `OutOfUsage`, `Rebegin` (`KeepRebegin`) | the exit report with `out_of_usage` (`claims.rs` `apply` -> `requeue_turn`) and the actor's claim by id (`core_runs.rs`: "a requeue after a usage limit"); `requeue_turn` cancels a paused job's turn, the claim's paused check catches the rest |
| the turn row | `enqueue_turn_in` (`fire_in`, step 5), source `schedule` |
| who runs the turn (not in `Fire.tla`: `HostRuns.tla`, below) | the host's runner: `hostclaim.queued` (`hostclaim.rs`, unkeyed `schedule` turns, `redeemedBefore`, `settings`), `serve/host/src/routines/core_runs.rs` (pull after each `schedule.tick` and every 2 s in Writer mode; a turn redeemed before is cancelled "interrupted", a routine's turn begins by its id through the bot's actor, never enqueued, a cron job's goes to `core_cron.rs`). `RUNS_FIRED_TURNS` is off since #1035 (architect MUST 1); package F turns it on with the temp-home e2e, and this re-sync (HostRuns here, the old-tick rows) is F's MUST 6 |

## Who runs a fired turn (`HostRuns.tla`)

The architect's model of design 2026-11-16-core-queued-runner (his folder holds the original and its results), copied here
at the M4-5 re-sync, with package C's cron turn added. Turns: `f` (a routine's fired turn, run on the owner's bot), `h` (a
turn a host actor enqueued, keyed) and `c` (a kith-sched job's fired turn, run by `kith-sched run-turn`). One core restart,
one host restart, at most `MaxC` = 3 claims of `c`. The model header maps each action to the code. Properties:

| Property | Plain sentence | Harm |
|---|---|---|
| `PromptOnce` | A turn's prompt reaches an engine at most once, over all its claims. | A scheduled job runs twice for one slot; a message is answered twice. |
| `NoLiveKill` | A core restart never kills a live host turn. | Every daemon update kills the person's in-flight turn. |
| `NoStuck` | When nothing can happen, every turn is final and no claim is open. | A turn or claim waits for ever (and every later fire parks behind it). |
| `FiredRuns` | When nothing can happen, a fired turn ran (or was handed to an engine), or expired, or failed. | A fired turn is cancelled or left without ever running. |

| Rule | Code | Row that fails without it |
|---|---|---|
| R: the runner pulls unkeyed fired turns | `core_runs.rs` | `hr-no-runner` (`NoStuck`) |
| R: only unkeyed turns, never a redeemed one again | `hostclaim.queued`, `core_runs.rs` (`redeemedBefore` -> cancel "interrupted") | `hr-pull-any`, `hr-rerun-redeemed` (`PromptOnce`) |
| K: step 5 keeps the host's live claims | `reconcile.rs` `release_unlisted` | `hr-no-keep`, `hr-today` (`NoLiveKill`) |
| C: run-turn attaches with the check-and-set on its own claim before the engine starts | `sched_run::attach` (`claim.attach`), `kith-sched` run-turn exit 4 | `hr-no-cas` (`PromptOnce`: a run-turn spawned by the host before its restart attaches to the next host's claim, and the next host's run attaches too) |
| C: an attached run's claim is `external`: adopt and step 5 leave it, end_gone fails its turn when the process is gone | `warm.rs` adopt, `reconcile.rs`, `claims.rs` end_gone | no row; removing `EndGoneC` fails `hr-design NoStuck` (checked by hand at the re-sync) |

`hr-runner` is the architect's design row without the cron turn (830 states, as in his results). The model keeps one
`over` thread (`thr`): where a newer claim spawns while an older thread still waits (only without rule K), the older one
drops out of the model. `HostRuns.tla` does not
model: pause, revoke and rollback (they cancel a claim with no pid: `Fire.tla` and `Handover.tla`), the person check of
run-turn (exit 3 before it attaches; a run-turn that exits before it attaches is `DieC`, row `hr-dies`: the host's thread
cancels its turn, and `FiredRuns` does not count such a behaviour), a bad exit (failed, like `done` final), the bot actor's out-of-usage keep path (`Fire.tla` `routine`, `keep-nocheck`).

## The handover (M4-5; the model is `Handover.tla`)

`Handover.tla` models kith-sched, the host's routines (J2) and the core during the switch: the operator's step (pre,
import, tombstone, switched), the core's tick, and the rollback. Properties: `AtMostOnce` (no slot is covered by two
runs), `NoSilentLoss` (every slot a scheduler evaluated is covered or has a decided row) and `NoStack` (no job has two
running runs). Design and verdict: the architect's `2026-11-17-schedules-handover` folder (design.md, model/results.md).
The rules, each with the `h-*` row that fails without it:

| Rule | What | Code | Row |
|---|---|---|---|
| H1 `tick.lock` | kith-sched's tick and the step share one lock; the tick checks the tombstone under it | `kernel/bin/kith-sched` `tick_lock` (#1033); the step's `schedules_switch::hold_both` (#1039); gate condition 5 refuses a launchd kith-sched that does not honour it (#1043) | `h-no-lock` (`AtMostOnce`) |
| H2 anchor | J2 (a host routine) counts from its imported `next_run` (the anchor) until its first core plan, never from the watermark; the once row is the anchor too | `schedules/anchor.rs`, the step's import (#1039) | `h-no-anchor` (`AtMostOnce`, `NoSilentLoss`), `h-wm-back` (`AtMostOnce`) |
| H3 live old run | an imported kith-sched run that is still live counts as unfinished for the core's skip rule | the import writes `last` into `sched_runs`; `schedules/running.rs` (#1039) | `h-no-h3` (`NoStack`) |
| H4 per-job rollback | a rollback rewinds only the job it concerns: its next run counts from where it would next count (the anchor until used, never below anchor - 1), the core's cancelled runs leave a `skipped:rollback` row, a running core run is written into `state.json`'s `last` last | `cutover/modules/schedules_back.rs` (#1042) | `h-perjob-as-written` (`AtMostOnce`), `h-global-rewind` (`AtMostOnce`), `h-bare` (`NoSilentLoss`), `h-nolast` (`NoStack`) |
| condition 6: old ticks | an old-code tick (no `tick.lock`) that launchd started before the update moved `cli/current` may still be running when the step takes the lock; right after taking it, the step lists this home's tick processes and waits until each is gone (refuses at `TICK_WAIT`, changing nothing) | package F: `schedules_switch::hold_both`, right after `tick.lock`, for the step (the rollback only notes old ticks) | `h-old-nowait` (`AtMostOnce`: the old tick passed its tombstone check before the step's import; the step's sha check (`ShaCheck`, below) still matches, the step tombstones, then the old tick renames over the tombstone and runs the slot the core runs too); `h-old` passes |
| sha check | the import records state.json's sha; just before the tombstone the step refuses, tombstoning nothing, when the file changed (a writer that did not take the lock); the operator runs it again | `schedules.rs` `before_tombstone` | none alone: it catches an old tick's write between the import and the check, not one after the tombstone, which is why condition 6 is needed |

Package C's change of the host's adopt for scheduled runs (pid alive: keeps running; redeemed then dead: ends and is
never retried; no pid: queued again) is in `Fire.tla` (`Cron`, `AdoptMode` "c") and in `HostRuns.tla` (turn `c`, with the
attach check-and-set); `Handover.tla` has no host restart mid-run (its `Begin`/`End` stand for `HostRuns.tla`).

The old tick (`OldTick`) assumes what launchd does: it never starts `dev.kith.sched` again while a tick of that label still
runs, so a new-code tick never overlaps the old one; and the old code is the KS path with `TickLock = FALSE` (kith-sched
at 1cf2b5f44: read with a tombstone check, save the tick by check-then-rename, start, record by check-then-rename). One old
tick; it starts (`OTBegin`) only before the update, but may read `state.json` after it (`OTRead`: prune, the ask sweep and the
wake drain come first). A `kith-sched run` or `tick` by hand with the old code is not modelled.

What makes `h-old`'s pass mean something on a Mac: condition 6's lister finds a tick by resolving the kith-sched path in
its argv now, and the launchd plist names `cli/current/bin/kith-sched` (the installer writes `stable_kernel()` on purpose,
`kith-sched` `install`), so an old tick's path resolves to the new program and is listed. A plist with a versioned path
would make condition 6 blind to old ticks, and `OldWait` would then describe nothing.

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
| `routine` | `Cron = FALSE`: a host routine's job | pass: `AtMostOncePerSlot`, `NoStacking`, `PausedNeverStarts` |
| `h-old` | `OldTick = TRUE`: an old-code tick still running at the step, condition 6 waits it out | pass: `AtMostOnce`, `NoSilentLoss`, `NoStack` |
| `h-old-nowait` | `OldTick = TRUE`, `OldWait = FALSE` | `AtMostOnce` |
| `hr-design`, `hr-runner`, `hr-dies` | `HostRuns.tla` as designed, with and without the cron turn, and with run-turn dying before it attaches (`RunDies`) | pass: `PromptOnce`, `NoLiveKill`, `NoStuck`, `FiredRuns` |
| `hr-today`, `hr-no-keep`, `hr-no-runner`, `hr-pull-any`, `hr-rerun-redeemed`, `hr-no-cas` | `Runner = FALSE` and `KeepLiveHost = FALSE`; `KeepLiveHost = FALSE`; `Runner = FALSE`; `PullOnlyUnkeyed = FALSE` and `SkipRedeemed = FALSE`; `SkipRedeemed = FALSE`; `AttachCAS = FALSE` | `NoLiveKill`; `NoLiveKill`; `NoStuck`; `PromptOnce`; `PromptOnce`; `PromptOnce` |
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
   entry path); or the rule changed on purpose. Update the model that describes it (`Fire.tla`, `Handover.tla` or
   `HostRuns.tla`, and its base consts) from the code, not from what makes a result pass. One step in code = one action; cite the function above the action.
3. `verification/schedule-fire/run.sh`. A row that differs is drift (replay the TLC trace against the code) or a bug.
4. A pass can have lost the ability to fail: break the new check in a scratch edit and see `run.sh` fail.
5. Only then set `SYNC` in `drift.sh` and the sync commit below.

**M4-5a notes (not modelled).**
- *Pruning vs `WatermarkRows`:* `sched_fires` rows older than 14 days (deliveries `event:d...`: 60) are deleted hourly, except a job's latest timed slot (an interval counts from it) and rows whose turn has not finished. `WatermarkRows` ("a row for every slot the watermark passed") is about slots between the old and the new watermark in one tick, never about slots weeks behind it: `plan` only looks forward from the watermark, so a pruned slot is never planned again. The limit is that a `manual:<key>` run-now key or a delivery id reused after its window fires again.
- *All-fail limit (10 ticks):* when nothing went through a tick with more than one job due, the failure may be the core's (a locked keychain, an unwritable database), so jobs get 10 failing minutes before each is given up on, against 3 when something else went through. It is a bounded hold, not tied to the 1-day catch-up window: that would hold the watermark (and make every tick plan from further back) for a day. Ten minutes covers a keychain prompt or a restart; longer outages are then recorded as `skipped:error` rows rather than silently replayed.
- *`mac` on a job:* a job naming another Mac is not planned, counted or recorded here. A webhook/event delivery (`not-here`, answered before the key is looked at) and run-now (refused) for such a job are refused too, with nothing written (M4-5b); this Mac's id is only read for the refusal, never made. A job with no `mac` is every Mac's.

- *Fairness of `RunnerCancelAny` (`RunnerCancel`, `RunnerGiveUp`):* it has none. `TurnsEnd` holds through `Expire`'s strong fairness, but nothing checks that a redeemed turn is cancelled rather than expired.
- *Not modelled (adopt):* core reconcile step 5's requeue of a claim whose process is gone (`HostAdopt`'s any-time requeue over-approximates it). The out-of-usage keep path re-begins a running turn by the same id inside the actor, so a paused job's running turn can continue on the backup account; deliberate per rule 5.

**Sync commit:** `65a96a5e244a5ec19bb8fffc07677aa7575b30bb` (F's head, after #1047 and #1044; M4-5 re-sync, architect MUST 6 of
package F. Read: #1033 (kith-sched `tick.lock`), #1039 (the step: host.lock, tick.lock, anchor, live run), #1042 (per-job
rollback hand-back), #1043 (gate condition 5), #1044 (cron turns as `kith-sched run-turn` with `claim.attach`).
`Fire.tla`: the base job is a cron job (`Cron`): its turns are claimed for any person, the keep path is a routine's only,
adopt keeps an attached run; a routine's job is the `routine` variant, whose runner cancels a turn of a person without
the owner's bot (`RunnerGiveUp`, core_runs.rs give_up); adopt's requeue cancels a paused job's turn (requeue_turn);
`keep-nocheck` runs on `routine`. `Handover.tla`: H1-H4 checked against the merged code (no change), the old-code tick
and gate condition 6 (`OldTick`, `OldWait`, rows `h-old`, `h-old-nowait`, F review SHOULD 7), and the step's sha check
before the tombstone (`ShaCheck`, `StepCheck`: in code since #1039, missing from the model; `h-*` state counts grew a little
with it, every row as before). `HostRuns.tla` brought in
from the architect's folder with the cron turn `c` (rows `hr-*`; `hr-runner` reproduces his 830 states). Before the
change the whole table was as expected on CI at fdd84be16 (#1044's head, same model files); after it, `VERIFY_JOBS=1
run.sh` was run in full here and every row is as expected. Breaks checked by hand: no `EndGoneC` fails `hr-design
NoStuck`. Condition 6 itself lands in package F; re-check `OldWait` against its code there.)

## Map of the folder

- `Fire.tla`: the model. `base.consts`: the switches for the design. `variants/*.set`: overrides per variant.
- `Handover.tla`: the M4-5 handover model (variants `h-*`, switches in `base.handover.consts`), copied from the architect's folder.
- `HostRuns.tla`: who runs a fired turn (variants `hr-*`, switches in `base.hostruns.consts`), copied from the architect's folder.
- `checks.tsv`: the table `run.sh` enforces. `run.sh`: pinned tools and the table. `drift.sh`: sync commit and covered files.
