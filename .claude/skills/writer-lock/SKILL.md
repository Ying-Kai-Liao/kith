---
name: writer-lock
description: Model-checks (TLA+/TLC) the conversation writer lock and the holder's claim rules - one engine per conversation across a holder crash, a leaked lock fd, a revoke that kills the lock holder, park and wake, take-over, and an idle warm process when a terminal turn arrives. Use when a change touches core/stub (the start stub and its lock), core/holder/src/holder.rs (spawn, reap, ACK, KILL, the highest claim seen), core/holder/src/proto.rs, core/runtime/src/holder_link.rs, core/runtime/src/terminal.rs, or the coming core/runtime/src/conversations/ (claim transaction, claim ids, redeem, park and wake, take-over), or when a new path starts an engine on a conversation. Also use to answer "can two engines write to this conversation?".
---

# Writer lock and holder claims: one engine per conversation

Design: `docs/plans/runtime-design.md` section 4 (the writer lock, the round-4 rules, the start protocol) and
section 11 (open). The models are `verification/writer-lock/WriterLock4.tla` (the lock: stub, warm turns, direct
runs, take-over writer, core and holder crashes) and `verification/writer-lock/Holder.tla` (the holder's `spawn`
decision, exits kept until acked, and the core's view of holder ids across a core or holder restart). Both were
written by the architect and run green in his own folder; M3-0a committed them here so CI re-checks them and code
drift shows. Their origin (path, date) is in each file's header. `results4.md` is the architect's result table plus
what M3-0a added. This skill is the only other documentation.

State today: the stub's lock primitive exists (`core/stub/src/lib.rs`), and since M3-4 the holder runs every Pipes
engine under `kith-stub --resident` and relays the core's `take`/`release` for a turn to it (claim id on every frame;
the holder acts only on the current claim or a take in flight; unacked answers are kept per claim and kind, a weaker
answer never replacing a stronger; a dead stub ends its engine). What is still missing is the core's claim transaction
with the redeem (M3-2, M3-5). The holder's relay rules (in-flight take, report keeping) are bookkeeping on top of the
modelled safety properties and are not in `Holder.tla`; model them at M3-5. A green run
says the design holds; it does not say the code follows it. `drift.sh` lists the code that exists and tells you when
it moves. `Holder.tla` was derived from the holder at 73a03656; `WriterLock4.tla` models the design only.

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

**Pinned at `73a03656ad207a7abc854136eda0ab848a2ab591`** (the rust-rebuild commit `Holder.tla` was derived from; not
"confirmed": the covered files have changed since and nobody has re-read them against the models, so `drift.sh` lists
them on purpose). The same sha is in `drift.sh` (`SYNC=`). Move it only after you have re-read the changes and
confirmed the models describe the code at the new commit, and change both places.

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
| `holder-crash-minimal` | HighWater, WriterLock, RedeemChecksClaim, HolderCrash, StaleSends | pass | `OneWriter` and `RunOnce` hold without `RefuseOlder`, `KeyByInstance`, `RecordAtSpawn`. Whether those three matter for `DoneTruth` cannot be judged yet: `DoneTruth` fails for every combination (masked by the open finding below), so there is no row for them. |
| `holder-crash-nolock` | crash-all minus WriterLock | `RunOnce` fails | The lock is what keeps a respawned stale claim out after a holder restart (the high-water mark resets with the holder). |
| `holder-crash-noredeem` | crash-all minus RedeemChecksClaim | `RunOnce` fails | The stub's redeem must refuse a claim that is not core.db's current one. |
| `holder-crash-nohighwater` | crash-all minus HighWater | `RunOnce` fails | |
| `holder-crash-donetruth` | crash-all, `DoneTruth` too | `DoneTruth` fails | **Open finding, see below.** |

### Open finding: `DoneTruth` fails with a holder crash (every combination of switches)

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

Not done in M3-0a: adding the redeem write to `Holder.tla`, a broken variant that fails `DoneTruth` without it, and
`DoneTruth` rows for `KeyByInstance` and `RecordAtSpawn`. Until then `holder-crash-donetruth` stays `fail` on purpose.
If you make it pass, move the row and say what rule you added; do not drop the invariant.

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

- Trace checking (design "The oracle after Python"): `Runtime.tla` and the event replay are M3.
- The TLA+ lease model is M3-0b (`Threads.tla` is retired or resynced in M3-15).
- The slow rows in the PR job, if a runner with more cores is used.
- The redeem write in `Holder.tla` and the rows above (the open finding).
- `killidle` liveness, with a bounded wait for the kill (a terminal that waits for a kill while the holder is down).
- An ack-after-settle switch in `Holder.tla` (the ACK only after the core has settled the exit).
- `r4-fix-1d-safety` (no recorded result) and the cfgs that ran out of heap, if a bigger heap is ever used.

## Map of the folder

- `WriterLock4.tla`, `r4-*.cfg`: the lock model and its runs (architect's; the `M3-0a` lines are the Q2 variant).
- `Holder.tla`, `holder-*.cfg`: the holder model (architect's) and the runs M3-0a added.
- `results4.md`: the architect's result table, plus what M3-0a added.
- `checks.tsv`: the table `run.sh` enforces. `run.sh`: runner (tools from `verification/threads/run.sh --tools`).
  `drift.sh`: sync commit and covered files.
