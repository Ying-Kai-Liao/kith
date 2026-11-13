---
name: writer-lock
description: Model-checks (TLA+/TLC) the conversation writer lock, the holder's claim rules and the core's claim and turn rows - one engine per conversation across a holder crash, a leaked lock fd, a revoke that kills the lock holder, park and wake, take-over, a stop of an ask (the stopped-ask fence, a wake given back, a card resume) and an idle warm process when a terminal turn arrives. Use when a change touches core/stub (the start stub and its lock), core/holder/src/holder.rs (spawn, reap, ACK, KILL, the highest claim seen), core/holder/src/proto.rs, core/runtime/src/holder_link.rs, core/runtime/src/terminal.rs, core/runtime/src/conversations/claims.rs (claim transaction, claim ids, redeem, stop, revoke, park and wake, take-over), the stop in core/runtime/src/asks/mod.rs (end_tree) or the give-back in core/runtime/src/wake/queue.rs, or when a new path starts an engine on a conversation. Also use to answer "can two engines write to this conversation?".
---

# Writer lock and holder claims: one engine per conversation

Design: `docs/plans/runtime-design.md` section 4 (the writer lock, the round-4 rules, the start protocol) and
section 11 (open). The models are `verification/writer-lock/WriterLock4.tla` (the lock: stub, warm turns, direct
runs, take-over writer, core and holder crashes) and `verification/writer-lock/Holder.tla` (the holder's `spawn`
decision, exits kept until acked, and the core's view of holder ids across a core or holder restart). Both were
written by the architect and run green in his own folder; M3-0a committed them here so CI re-checks them and code
drift shows. Their origin (path, date) is in each file's header. `results4.md` is the architect's result table plus
what M3-0a added. This skill is the only other documentation.

State today (M3-12, 2026-11-14): the stub, the holder (every Pipes engine under `kith-stub --resident`, claim ids on every frame),
the core's claim transaction, the holder link (claim ids, exit reports, turn lock steps, kill re-send, hello's step 7), the
wake turns and the host's core claims (`KITH_HOST_CORE_CLAIMS`, off by default) exist; the holder link's per-turn lock steps (`begin_turn` / `finish_turn`, M3-5) are built but have no production caller yet; the
host's claim files are still the writer until the cutover. The holder's relay rules (in-flight take, report keeping) are bookkeeping on top of the modelled
safety properties and are not in `Holder.tla`. A green run says the design holds; it does not say the code follows it.
`drift.sh` lists the code that exists and tells you when it moves. `Holder.tla` was derived from the holder at 73a03656 and,
for the core's claim record (switch `RedeemWrite`), from `claims.rs` (M3-2); `WriterLock4.tla` models the design only;
`Claims4.tla` is derived from the claim code and was re-read at 1720d27e.

## Run it

```
verification/writer-lock/run.sh                       # the fast rows of checks.tsv, about 9 min on 4 cores once the tools are cached
verification/writer-lock/run.sh --all                 # also the 2 slow rows (15-30 min each)
verification/writer-lock/run.sh --show MODULE CFG     # TLC's full output (the counterexample) for one row
verification/writer-lock/drift.sh                     # which covered files changed since the sync commit
```

CI runs `run.sh` (job `writer-lock`) on pull requests and pushes that touch `verification/writer-lock/`, this skill,
or the covered code (the files `drift.sh` lists). The two slow rows (`r4-fix-survive-safety` 39.4M states,
`r4-fix-die-safety` 22.8M states) are not in the PR job; run them with `--all` before moving `SYNC` or after changing
`WriterLock4.tla`.

`run.sh` exits 0 only when every row (module, cfg, expected pass/fail, the invariant that must be violated, speed) of
`checks.tsv` came out as expected. A `fail` row names the violated invariant, or `temporal` for a PROPERTY (P3).
Tools are the same pinned, sha256-checked ones as the thread-writers skill (it supplies them: TLA+ tools v1.7.4,
the architect's version; Temurin JRE 21 only when no Java 11+ is on PATH; cached under
`${XDG_CACHE_HOME:-$HOME/.cache}/kith-verify/`).

## Sync commit

**Pinned at `1720d27eef3ac4f1208b94fa7c61f268a9e824b4`** (rust-rebuild, 2026-11-14, the M3-12 gate): `drift.sh` was read change
by change from the old pin (73a03656) and every covered file's change checked against the three models; `results4.md`
("Added by M3-12") has each change and its verdict. The same sha is in `drift.sh` (`SYNC=`). Move it only after you have
re-read the changes and confirmed the models describe the code at the new commit, and change both places.

## What is promised, and the harm if not

| Property | Model | Plain sentence | Harm |
|---|---|---|---|
| `P1_NoTwoWriters` | WriterLock4 | At most one of: a warm turn, a direct run, the take-over writer, writes the conversation. | Two engines on one session fork or corrupt it. |
| `P2_NoLostTurn` | WriterLock4 | A claimed or running turn holds the claim; nothing else drops a turn. | A turn silently lost. |
| `P3_Liveness` | WriterLock4 | Every queued turn ends done or cancelled (weak fairness): busy parks it and a level-triggered re-probe wakes it. | A turn waits for ever behind a lock nobody holds. |
| `P4_GoneMeansEmpty` | WriterLock4 | The holder reports "gone" only when the engine group is empty. | A killed run still writing after the claim is free. |
| `P5_StaleNeverPrompted` | WriterLock4 | A warm process is never prompted after the transcript changed behind it (hash check at turn start). | The agent answers without what the terminal did. |
| `P6_LockFileNeverDeleted` | WriterLock4 | No action deletes the lock file. | A lock on an unlinked inode is a second writer. |
| `P7_NoWarmBesideRegisteredTerminal` | WriterLock4 (M3-0a) | A terminal run registered with the core never runs beside a warm process of the conversation (Q2: the core kills the idle one first). | Idle warm process kept past a terminal turn. |
| `OneWriter` | Holder | At most one engine process of a conversation holds the writer role. | As P1. |
| `RunOnce` | Holder | A claim's job becomes the writer at most once. | A respawn of an acked claim writes beside the next one. |
| `DoneTruth` | Holder | When the core takes its claim as over, no process of that claim still writes. | Claim released under a running engine. |

## Rows, and what each says

Names are `cfg` in `checks.tsv`. `run.sh --show WriterLock4 r4-nohash` prints the counterexample.

**WriterLock4, fix (pass).** `r4-fix-{survive,die}-live-{revoke,takeover}`: invariants plus P3 with one direct run and
a revoke or a take-over; `r4-fix-{survive,die}-safety` (slow): P1, P2, P4, P5, P6 with two direct runs, one revoke, one
take-over. `survive` = the holder's group outlives a holder crash, `die` = it dies with it. Bounds: 1 conversation,
2 turns, 1 core crash, 1 holder crash, 1 escaped child.

**WriterLock4, broken variants (each fails).**

| cfg | Removed | Fails |
|---|---|---|
| `r4-nohash` | the transcript-hash check before prompting a warm process | `P5` |
| `r4-readlate` | the core records the hash at the ack, not the turn end | `P5` |
| `r4-stubfirst` | the stub waits for its engine before exiting | `P1` |
| `r4-goneearly` | "gone" only once the group is empty | `P4` |
| `r4-nostub` | the resident stub holds the warm-turn lock (holder holds it instead) | `P1` |
| `r4-inherit` | the lock fd is CLOEXEC (a child the engine starts inherits it and wedges the conversation) | `P3` |
| `r4-edge` | level-triggered wake (wake only on a "released" event, lost on a core crash) | `P3` |

`r4-parkclaim-*` (busy parks the turn but keeps the claim) pass: no modelled property needs "park with no claim"; it
stays for reasons the model does not check (a revoke has nothing to chase; the conversation does not look claimed).
Its full-scope run ran out of memory (see `results4.md`); the three small ones are in the table.

**Q2, the idle warm process (M3-0a).** Decided default (2026-11-04): kill it first. Variant `killidle`
(`r4-killidle`, pass, 4.6M states) adds `CoreKillIdle` and makes a terminal register only once the group is empty;
`r4-keepidle` (the "fix" design, idle process kept) fails P7 on purpose: it shows P7 is not vacuous, not that keeping
the idle process is unsafe (P1 to P6 hold either way). Modelled with the existing kill (a revoke's: `killReq` then
"gone"), no new message. Not modelled: a terminal run that finds no core (the `--direct` fallback) cannot kill
anything, so the hash check (P5) is what protects the conversation then.

**Holder** (`holder-*.cfg`; the architect left no cfgs, these are M3-0a's: `MaxClaim=3, MaxInst=2, MaxPid=3`; flags
in the cfg). `RefuseOlder`, `HighWater`, `RefuseWhileRunning`, `KeyByInstance`, `RecordAtSpawn`, `WriterLock`,
`RedeemChecksClaim`, `HolderCrash`, `StaleSends` are the model's switches.

| cfg | Switches on | Result | What it says |
|---|---|---|---|
| `holder-code` | none | `RunOnce` fails | Without a memory of acked claims, a spawn for the current claim after its exit was acked starts a second process. |
| `holder-code-stale` | StaleSends | `RunOnce` fails | The same with a late spawn for an older claim. |
| `holder-refuseolder` | StaleSends, RefuseOlder | `RunOnce` fails | Refusing below the highest claim *held* is not enough: the entry is gone after the ack. |
| `holder-refusewhilerunning` | RefuseWhileRunning | `RunOnce` fails | Same hole. |
| `holder-highwater` | StaleSends, HighWater | pass | `core/holder/src/holder.rs` already keeps "the highest claim seen per conversation, kept after the process is acked". |
| `holder-lock` | + WriterLock | pass | The stub's lock on top. |
| `holder-crash-all` | all guards + HolderCrash | pass (`OneWriter`, `RunOnce`) | A holder restart does not break them. |
| `holder-crash-minimal` | HighWater, WriterLock, RedeemChecksClaim, HolderCrash, StaleSends | pass | `OneWriter` and `RunOnce` hold without `RefuseOlder`, `KeyByInstance`, `RecordAtSpawn`. For `DoneTruth` see the `holder-crash-donetruth-*` rows. |
| `holder-crash-nolock` | crash-all minus WriterLock | `RunOnce` fails | The lock is what keeps a respawned stale claim out after a holder restart (the high-water mark resets with the holder). |
| `holder-crash-noredeem` | crash-all minus RedeemChecksClaim | `RunOnce` fails | The stub's redeem must refuse a claim that is not core.db's current one. |
| `holder-crash-nohighwater` | crash-all minus HighWater | `RunOnce` fails | |
| `holder-crash-all-write` | crash-all + `RedeemWrite` | pass (`OneWriter`, `RunOnce`) | The redeem write does not break them. |
| `holder-crash-donetruth` | crash-all + `RedeemWrite`, `DoneTruth` too | pass | **Was the open finding (below), fixed by the redeem write of M3-2**: the redeem CAS records the pid, a release on a new instance is a CAS, a running claim goes to `ending` until the recorded pid is gone. |
| `holder-crash-donetruth-nowrite` | the same, `RedeemWrite` off | `DoneTruth` fails | The original finding: the omniscient core releases the claim under the orphan's engine. |
| `holder-crash-donetruth-nocas` | `RedeemWrite`, `RedeemCAS` off | `DoneTruth` fails | Broken variant: the stub starts its engine even when the claim was released (a late redeem succeeds). |
| `holder-crash-donetruth-release` | `RedeemWrite`, `HoldRunning` off | `DoneTruth` fails | Broken variant: an unlisted running claim goes straight to `ended`. |
| `holder-crash-donetruth-nokey` | `RedeemWrite`, no `KeyByInstance` | pass | With the record, `DoneTruth` does not need the instance-keyed ids. |
| `holder-crash-donetruth-norecord` | `RedeemWrite`, no `RecordAtSpawn` | pass | Nor the pid recorded at spawn: the redeem's record is what counts. |
| `holder-crash-donetruth-minimal` | `RedeemWrite`, only HighWater, WriterLock, RedeemChecksClaim | pass | None of `RefuseOlder`, `KeyByInstance`, `RecordAtSpawn` is needed for `DoneTruth` (the code keeps `spawned` pid recording anyway as a cheap extra). |

Code for the `RedeemWrite` actions: `core/runtime/src/conversations/claims.rs` (`redeem` = `TakeLock`'s CAS,
`release_unlisted` = `Connect`'s release, `report` = `Settle` with the pid check (a report without the recorded pid applies nothing), `end_gone` = `KillRecorded` +
`ConfirmGone`). Not modelled: revoke and stop (`stop`, `revoke_person` also go to `ending`), the stub's lock and redeem as
two steps, a queued take-over claim.

### Finding (closed by M3-2): `DoneTruth` fails with a holder crash (every combination of switches)

Steps (`run.sh --show Holder holder-crash-donetruth`): the core takes claim 1 and sends SPAWN; the holder starts the
stub and answers, the reply is lost with the connection; the holder crashes (its entries are memory); the stub, an
orphan, takes the conversation lock and redeems claim 1 (it is still current); the core meets the new holder
instance, finds no entry for claim 1 and no recorded pid for it (the reply was lost, so nothing was recorded at
spawn), and releases the claim (`curDone`) while that engine writes. Every combination of the nine switches in a
512-run grid failed `DoneTruth` once `HolderCrash` was on. What the model does not have: the redeem write. The rule M3-2 must state (and the model must then gain):

- **Redeem** is a core.db CAS `claimed -> running`, only if the claim is current and not released, recording the
  redeemer's pid and start time and the holder instance.
- **Release on a new holder instance** is a CAS too: a `claimed` claim becomes released, so a later redeem fails.
- A `running` claim the new instance does not list is **never released** until the recorded pid is killed and
  confirmed gone (or the core can take the writer lock non-blockingly).
- `WriterLock4.tla`'s omniscient `Orphan` (the core knowing a surviving group) should be replaced by the same record.

M3-2 added the redeem write to `Holder.tla` (switches `RedeemWrite`, `RedeemCAS`, `HoldRunning`), the broken variants and the
`DoneTruth` rows above; the first three rules are in `claims.rs` with tests (`holder_crash_donetruth_*`) and a proptest of
random step sequences. Still open: the fourth rule. `WriterLock4.tla` keeps its omniscient `Orphan`; replacing it needs a
recorded "redeemed under instance" bit in a model whose slow rows take 15-30 min each and were not re-run here.
A `done` or `exit` that names another pid ends a running or ending claim in neither the code nor the model (`Settle`'s `h.pid = cpid`). One that names no pid ends nothing either: in the code a claim with a recorded pid moves to `ending` (still the writer, `apply` in claims.rs) and `end_gone` ends it once the pid is gone, which the model leaves out as a stutter (`ending` is already the writer there, and `ConfirmGone` is the end); reports are keyed by `(claim_id, kind, pid)` so the writer's own is never shadowed. A `--direct` claim (kind `external`) is outside the model: reconcile and a holder change skip it and only `end_gone` ends it.

## Claims4: the claim and turn rows against the processes (M3-10b1)

`Claims4.tla` is the core.db side of round 4: the claim rows (`queued claimed running ending ended`), the turn rows
(`new queued parked claimed running done cancelled`), the ask (`open stopped`) and the processes (stub, engine, the holder's kept
report), one function of `claims.rs` per action. It is where **Stop, the give-back of a carried wake and a card resume**
are modelled (the architect called it `Runtime.tla`; no such file exists, and the lock model has no turn sources, so they
live here and not in `verification/threads`). A wake given back and a card resume are the same step for the claims layer: a new
turn of source `ask` or `wake` (`TurnArrive` of an `Auto` turn) for the conversation of an ask that was already stopped.
The module does not model the lock itself (WriterLock4 does): "the lock is taken" = no other claim's process is writing.
Bounds: 1 conversation, 2 turns (turn 2 is an ask/wake turn), 4 claims, 1 holder crash, 1 revoke, 1 take-over.

Code, cited at `origin/rust-rebuild` cf308ebc (`core/runtime/src/...`; line numbers are from then, M3-12 re-read the changes since: `results4.md`):

| Action | Code |
|---|---|
| `MayCheck`, `TurnArrive` | `revoke_epoch` conversations/claims.rs:182 before the may check; `enqueue_turn` :959 |
| `Park`, `Unpark` | `park` :1016, `unpark_ready` :1040 (no live claim, lock probed free) |
| `Claim` (epoch, fence, one writer) | `claim_in` :257: reconciled :268 (the only gate; no holder check), epoch :273, the stopped-ask fence :292-309 (`Error::Stopped`), the one-writer check :311-324, the turn CAS :349 |
| `Spawn`, `SpawnedRep` | the holder starts the stub; `apply` kind `spawned` :558 |
| `Redeem`, `Busy`, `EngineExit`, `Report` | `redeem` :374, `apply` busy :534 and done/exit :488 (fn :484), `report` :416 |
| `AskStop` | asks/mod.rs `end_tree` :581 -> `stop_conversation_in` claims.rs:640 -> `stop_in` :593; wakes withdrawn (`withdraw_owed_in`, wake/mod.rs:101) |
| `Revoke` | `revoke_person` :674 |
| `TakeOver`, `PromoteTW` | a queued claim (`state = 'queued'`, kind terminal); **promotion is not in the code** (claims.rs header: M3-11); modelled as the rule it must follow. `TakeOver` also cancels the live turn and stops its claim: that is the M3-11 rule, not code. It queues one claim at most (`claims_one_queued`, m0080) |
| `SendKill`, `HKill`, `EndGone` | the `Kill` the stops return, re-sent; `end_gone` :739 |
| `HCrash`, `HUp`, `Meet` | `different_holder` :922 -> `release_unlisted` :779 |
| give-back | wake/queue.rs `give_back` :288, `release` :296: only the wake's own bookkeeping; what restarts the conversation is a new turn, which `claim_in`'s fence refuses |

Properties (`INVARIANTS` and `PROPERTIES` of the cfgs): `OneLive` (the unique index), `OneWriter`, `WriterHasClaim` (a writing
process's claim is `running` or `ending`, never `ended`), `StoppedNeverRuns` (after the stop no ask or wake turn of that ask is
claimed or running), `RevokedQuiet`, `CancelledStays` (action: a cancelled turn stays cancelled), `AskSparesTakeOver` (action:
the ask stop leaves the owner's take-over claim alone), `EndingEnds` (every `ending` claim ends once the kill gets through),
`P3` (a turn the claims layer accepts ends done or cancelled), `P3Strict` (the same for every queued turn).

The temporal and action rows (`c4-nokillresend`, `c4-doneoverwrites`, `c4-stopall`) each check one PROPERTY, so `checks.tsv`'s `temporal` / `action` pins the reason.

Rows (`c4-*`): `fix-die` / `fix-survive` = the code with the holder's group dying with it or surviving a holder crash (180k and
about 320k states); `fix-doubletake` = two take-overs, no crash. Broken variants, each failing:

| cfg | Removed | Fails |
|---|---|---|
| `c4-nofence` | the stopped-ask fence in `claim_in` (a given-back wake or a card resume claims) | `StoppedNeverRuns`: the stopped child runs again |
| `c4-noepoch` | the revoke-epoch compare in `claim_in` | `RevokedQuiet` |
| `c4-endearly` | `end_gone` waiting for the process | `WriterHasClaim` |
| `c4-doneoverwrites` | `turn.state IN (claimed, running)` guard on a late `done` | `CancelledStays` |
| `c4-nokillresend` | the kill owed again by `different_holder` / re-sent after a holder crash lost it | `EndingEnds` (a hung engine, holder crashed after the kill left) |
| `c4-promoteearly` | promotion of a queued take-over claim waiting for no live claim (the index is the guard) | `OneLive` |
| `c4-stopall` | `stop_conversation_in` sparing a person's (take-over) claim | `AskSparesTakeOver` |
| `c4-fenceleak` / `c4-epochleak` | nothing (the code): a refused turn stays queued; `MaxRev=1` for the epoch | `P3Strict` (the `c4-fencecancel` / `c4-epochcancel` rows, where the caller cancels it, pass) |
| `WriterLock4 r4-tonolock` | the take-over writer taking the lock | `P1_NoTwoWriters` |
| `c4-hostconst` (M3-12, `HostHolder`) | one constant host instance (core_claims.rs `HOST_INSTANCE` today): a restarted host reads the old claim as its own and reports it done under the old stub's live engine. `c4-hostholder` (per-start instance, the old claims stopped to `ending` and killed, the group surviving) passes. The design's "fails P4" is `WriterHasClaim` here: the claim ends while the engine writes | `WriterHasClaim` |
| `c4-pidonly` (M3-12) | `end_gone` waiting for the pid only, not the process group it led (K2): a stub killed alone while the holder is down leaves a writing `orphan` engine | `WriterHasClaim` (`c4-orphan`, the code, passes) |

**Round-4 list to rows**: holder crash with the stub dying `r4-fix-die-*`, `c4-fix-die`; with it surviving `r4-fix-survive-*`,
`c4-fix-survive`; broken: `r4-nostub` (the lock dies with the holder), `r4-stubfirst`. Leaked lock fd: `r4-inherit` fails
(the fix passes in every `r4-fix-*` row with an escaped child). Revoke kill: `r4-goneearly`, `c4-endearly`, `c4-nokillresend`.
Park and wake (q217/q218): `r4-edge`, `r4-parkclaim-*`, `c4-fix-*` (`P3`). Take-over: `r4-fix-*-live-takeover`, `r4-tonolock`,
`c4-promoteearly`, `c4-fix-doubletake`. Stop / give-back / card resume: `c4-nofence`, `c4-fix-*`.

### Findings for the claims switch (S2), from Claims4

1. **A turn that `claim_in` refuses stays `queued` for ever**, for both refusals. The stopped-ask fence: `c4-fenceleak` fails
   `P3Strict`, `c4-fencecancel` (the refusal also cancels the turn) passes. The revoke epoch (claims.rs:273): `c4-epochleak`
   (`MaxRev=1`: the may check read epoch 0, the revoke ran, then the turn arrived) fails `P3Strict`, `c4-epochcancel` passes.
   `claim_in` returns `Err` and its transaction rolls back, so the cancel cannot sit inside it after the refusal: it has to
   be in the caller, in a transaction of its own, on that refusal (the fence now has its own `Error::Stopped` so a drain can
   tell it from a retry; the epoch refusal is still a plain `Refused`). Nothing else moves the turn (`expire_parked` only
   with a deadline), so the claimer would retry it for ever.
2. **Promoting a queued take-over claim must wait for no live claim** (`c4-promoteearly` fails `OneLive`): the index would also
   refuse it, but the caller sees an error and the take-over is lost unless it retries. M3-11 must promote on `ended`, not
   insert-and-hope.
3. **`different_holder` and `reconcile` end a `claimed` terminal claim (`holder_changed`) like a turn's, and `requeue_turn`
   finds no turn**: a take-over claim caught by a holder change is simply gone. The terminal entrance must retry it.
4. The kill must be owed again by `different_holder` / `reconcile` for an `ending` claim and re-sent until `end_gone`
   (`c4-nokillresend`): `release_unlisted` does it (`out.kill.push`); keep it when the holder link is rewritten (M3-5).
5. `claim_in` lets a turn claim while a take-over claim is `queued` (only `claimed/running/ending` block). Not a safety
   problem; the take-over can wait behind later turns. Decide whether a queued take-over blocks new turn claims.

## When you change covered code

1. `verification/writer-lock/drift.sh`. Nothing listed: no work. Listed files: read each diff and commit message,
   including code that moved and new code that starts an engine, takes a claim or ends one.
2. Decide per change: no effect; the model no longer matches (a step split in two, a check moved or removed, a new
   path to start an engine); or the promise changed on purpose (cite the doc). Update the `.tla` from the code, not
   from what makes a result pass. One step in code = one action; cite the function above the action.
3. `verification/writer-lock/run.sh` (and `--all` after touching `WriterLock4.tla`). A row that differs is drift (the
   model is wrong: replay the counterexample against the code) or a bug (every step can happen: report it with the
   steps in the code's words; do not weaken the property).
4. A pass on updated code can have lost the ability to fail: break the new check in a scratch edit and see `run.sh`
   fail. Add a broken variant (a switch or a `Variant` value) for a new check, and a row.
5. When M3-2/M3-4/M3-5 land, add their files to `FILES` in `drift.sh` and to the CI filter (`writer_lock` in
   `.github/workflows/ci.yml`), derive the claim-id, redeem, park and wake and take-over steps from the code, and
   close the open finding above.
6. Only then set `SYNC` in `drift.sh` and the sync commit above.

## Later

- Trace checking (design "The oracle after Python"): the event replay is M3. `Runtime.tla` (the architect's name) does not exist: its Stop, give-back and resume are `Claims4.tla`.
- The TLA+ lease model is M3-0b (`Threads.tla` is retired or resynced in M3-15).
- The slow rows in the PR job, if a runner with more cores is used.
- `WriterLock4.tla`: replace `Orphan` with the redeem record, then run `--all`.
- `killidle` liveness, with a bounded wait for the kill (a terminal that waits for a kill while the holder is down).
- An ack-after-settle switch in `Holder.tla` (the ACK only after the core has settled the exit).
- `r4-fix-1d-safety` (no recorded result) and the cfgs that ran out of heap, if a bigger heap is ever used.

## Map of the folder

- `WriterLock4.tla`, `r4-*.cfg`: the lock model and its runs (architect's; the `M3-0a` lines are the Q2 variant).
- `Claims4.tla`, `c4-*.cfg`: the claim and turn rows against the processes (M3-10b1; derived from `claims.rs`, `asks/mod.rs`, `wake/queue.rs` at rust-rebuild cf308ebc, re-synced at 1720d27e by M3-12).
- `Holder.tla`, `holder-*.cfg`: the holder model (architect's) and the runs M3-0a added.
- `results4.md`: the architect's result table, plus what M3-0a added.
- `checks.tsv`: the table `run.sh` enforces. `run.sh`: runner (tools from `verification/threads/run.sh --tools`).
  `drift.sh`: sync commit and covered files.
