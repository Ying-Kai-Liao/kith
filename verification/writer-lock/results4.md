# Writer lock, round 4 (WriterLock4.tla)

The refined writer lock from runtime-shapes design.md s.4. It covers:
- a resident stub that holds the lock fd with CLOEXEC;
- busy parking the turn with no claim, woken by a level-triggered re-probe;
- the transcript-hash check on a stale idle warm process;
- a holder crash, whether its group survives or dies;
- a child that escapes the group;
- a revoke that kills the lock holder ("gone" only once the group is empty);
- a take-over.

The model was written by the modeller and finished by the architect (the modeller hit its rate limit before any results). Every run below was made by the architect through tools/tlc.sh (TLC 1.7.4, 512 MB heap, 1 worker), on 2026-11-03 and 2026-11-04.

Bounds: 1 conversation, 2 turns, 1 core crash, 1 holder crash, 1 revoke, 1 take-over, 1 escaped child, and 1 or 2 direct runs.

## Fix (Variant "fix")

| Config | HMode | Directs | Revoke / take-over | Checks | Result | Distinct states |
|---|---|---|---|---|---|---|
| r4-fix-survive-safety | survive | d1, d2 | 1 / 1 | P1, P2, P4, P5, P6 | pass (depth 74, 30 min) | 39,444,026 |
| r4-fix-survive-live-revoke | survive | d1 | 1 / 0 | invariants + P3 liveness | pass | 1,536,823 |
| r4-fix-survive-live-takeover | survive | d1 | 0 / 1 | invariants + P3 liveness | pass | 716,486 |
| r4-fix-die-safety | die | d1, d2 | 1 / 1 | P1, P2, P4, P5, P6 | pass | 22,820,510 |
| r4-fix-die-live-revoke | die | d1 | 1 / 0 | invariants + P3 liveness | pass | 781,021 |
| r4-fix-die-live-takeover | die | d1 | 0 / 1 | invariants + P3 liveness | pass | 306,224 |

The properties:
- P1: no two writers.
- P2: no lost turn.
- P3: every queued turn ends done or cancelled (weak fairness).
- P4: "gone" means the group is empty.
- P5: a stale process is never prompted.
- P6: the lock file is never deleted.

**Not checked:** liveness with 2 directs, or with a revoke and a take-over in the same run. Each ran out of the 512 MB heap during the liveness pass:
- r4-fix-survive, both directs: no violation in 3.2M distinct states before running out.
- r4-fix-survive-live1d, with claim ids: no violation in 3.36M distinct states, 34k left on the queue.

tlc.sh fixes the heap size, so the liveness check is split by revoke and by take-over instead.

## Broken variants (each must fail)

| Config | Variant | Fails | States at the violation |
|---|---|---|---|
| r4-nohash | no transcript-hash check before prompting a warm process | P5 | 22,289 |
| r4-readlate | core records the hash at ack, not at the turn end | P5 | 22,519 |
| r4-stubfirst | stub may exit before its engine | P1 | 22,913 |
| r4-goneearly | "gone" reported once the kill is sent | P4 | 4,625 |
| r4-nostub | holder holds warm-turn locks itself | P1 | 3,037 |
| r4-inherit | engine holds the fd; a child inherits it | P3 | 165,537 |
| r4-edge | wake only on a "released" event | P3 | 8,132 |
| r4-parkclaim | busy parks the turn but keeps the claim | **no failure found** | see below |

parkclaim results:
- full scope: ran out of memory;
- 1 direct, no revoke or take-over: pass, 186,406 states;
- with a revoke: pass, 1,409,331 states;
- with a take-over: pass, 633,232 states.

So none of P1-P6 supports "park with no claim". It's a design choice for reasons the model doesn't check:
- a revoke has no claim or run token to chase;
- a parked claim doesn't keep the conversation looking claimed to the app, a scheduler or a registering `--direct`.

## Found while running (model fixes, not design changes)

1. **One report slot was overwritten.** A busy report replaced an unacked "done", the core acked only the busy, and turn 1 stayed running for ever. This was the first r4-fix-survive liveness failure, at 1,238,310 states.
   - Fix: a report is never overwritten until it is acked (HTryLock waits for an empty slot). That matches the design: exits stay in the holder until a core acks them (step 4).
2. **No claim ids.** The model named a claim by its turn number, but the design gives every claim a fresh id from a counter in core.db (steps 1 and 6). Round 1 verified that.
   - What went wrong: turn 2 was claimed, requeued by "gone", and claimed again while the first claim message was still in flight. The holder ran one message, then took the duplicate, tried the lock (its own stub held it) and reported busy. The core parked a turn that was running, and that repeated for ever. This was the r4-fix-survive-live1d liveness failure, at 910,028 states.
   - Fix: the model abstracts a claim id as "current or stale". A new claim makes every older one stale: in-flight messages, the holder's job, its unacked report and the running group's claim. The holder runs only a current job (the stub's redeem), and the core acts only on a report for the current claim.
3. **Reconcile matched by turn, not claim id.** After a core crash, `HeldBy` counted a stale job for the same turn as holding the current claim. The core kept the claim, the holder dropped the stale job, and the claim stayed claimed with no runner. This was a liveness failure at 941,742 states.
   - Fix: the holder lists its jobs, reports and running group by claim id (`HeldBy` compares ids).

The pre-fix model is kept as scratchpad WriterLock4.before-claimid.tla (not in this folder).

## Left out

- More than one machine or conversation.
- A warm process serving several conversations.
- Lost messages (only late and duplicate ones are modelled).
- More than one crash of each kind.
- Ctrl-C and the terminal's foreground group.
- The offline branch of a hand cut off from its brain.
- The lock folder's permissions; that's a filesystem rule, tested in code.

---

# Added by M3-0a (not the architect's)

Copied from `/Users/Kath/kith/agents/architect/files/kith/designs/2026-11-03-writer-lock/model/results4.md` (2026-11-04); the text above is unchanged. `run.sh` re-checks the table above and the rows below against `checks.tsv`.

- Pinned TLC is v1.7.4, the architect's version. Re-run on 2026-11-04 with the cfgs in this folder (1 worker for the fast rows, 1 GB heap): the distinct-state counts of the four liveness "Fix" rows and of the passing `parkclaim` rows are the same as above. The slow rows (`r4-fix-survive-safety`, `r4-fix-die-safety`) run only with `run.sh --all`. Not in `checks.tsv`: `r4-fix-survive`, `r4-fix-die`, `r4-fix-survive-live1d`, `r4-parkclaim` (they ran out of heap, see "Not checked") and `r4-fix-1d-safety` (no recorded result).
- Counts "at the violation" for `r4-edge` and `r4-inherit` differ (15,464 and 230,482 distinct states at the end of the run here); the verdict is the same.
- Open question s.11 Q2, an idle warm process when a terminal turn arrives, decided default "kill it first" (2026-11-04). Variant `killidle` in `WriterLock4.tla`: the core kills the idle warm group (`CoreKillIdle`, the same kill as a revoke's) and a terminal run registers with the core only once the group is empty. Property P7: a terminal run registered with the core never runs beside a warm process of the conversation.

| Config | Variant | Checks | Result | Distinct states |
|---|---|---|---|---|
| r4-killidle | killidle | invariants + P7 | pass | 4,580,689 |
| r4-keepidle | fix (keep the idle process) | invariants + P7 | P7 fails (13,090 states) | |

`keepidle` failing P7 is not a bug in the "fix" design: P1 to P6 hold with the idle process kept (the lock refuses a prompt while the terminal holds it, the hash check refuses a stale process). It shows P7 is not vacuous. Not checked: liveness of `killidle` (a terminal waiting for the kill while the holder is down never registers; P3 says nothing about direct runs).

---

# Added by M3-8 (writer-lock drift, Claims4)

`Claims4.tla` gains the warm turn's lock step and `LockStuck` (holder_link.rs `begin_turn` / `serve_turn` / `end_stuck_claim`) and the park for a terminal (wake/turns.rs `start_in`). TLC v1.7.4, 4 workers, 3 GB heap, 2026-11-05. The old Claims4 rows are unchanged (the new actions are off unless `Variant` is one of the warm ones; their verdicts are re-checked by `run.sh`).

What is modelled (process states `locked`, `stuck`, `stuckx`):
- `StubLock`: the stub takes the conversation's lock for the claim before the core is asked; `RedeemOK` is the redeem CAS; `GiveBack` is a refused redeem (the claim is not `claimed`), the lock goes back and the stub exits.
- `Stuck` (LockStuck: redeem failed or refused, and the give-back failed too). The code records the stub's pid on the claim first (a `spawned` report) and then stops it: the claim goes `ending` with a kill to send, and the holder is asked to kill the process by id (`requeue_end`, resent). `stuckx` is the corner where the claim was over already (ended unspawned): only the by-id kill is left, and a new instance's `Meet` owes it again.
- `parkterm`: a turn is parked only while a take-over (terminal) claim is `claimed`, `running` or `ending`, the q217 rule as `person_writer_live_in` counts it (a queued take-over is not a live writer, so it does not park a turn); nothing types into the terminal.

| Config | Variant | Checks | Result | Distinct states |
|---|---|---|---|---|
| c4-lockstuck-small | lockstuck, no holder crash, revoke or take-over | invariants (+ `StuckHasClaim`) + P3, EndingEnds, StuckEnds | pass | 137,697 |
| c4-lockstuck-revto | lockstuck, a revoke and a take-over, 3 claims | same | pass | 239,555 |
| c4-lockstuck-die | lockstuck, one holder crash that kills its group, 3 claims | same | pass | 246,228 |
| c4-lockstuck-survive | lockstuck, one holder crash the group survives, 3 claims | same | pass | 247,758 |
| c4-parkterm | terminal_live park | invariants + P3 | pass | 5,144 |
| c4-stuckdrop | the old `end_stuck_claim`: a claim with no pid ends at once while the stub holds the lock | `StuckHasClaim` | fails (71 states at the violation) | |
| c4-stucklog | the Err path only logs (no stop, no kill) | `StuckEnds` | fails (temporal; 818 states) | |

`c4-stuckdrop` is the expected violation of `StuckHasClaim`, the rule "the claim row is the lock's writer": the claim row ends while the stub still holds the lock, so the conversation's slot is free while the lock is not, and the turn goes through a new claim, a Busy and a `lock_busy` park instead of being requeued. It is a design rule, not a lock property: the old code did kill the process by id, and the same config with `OneHolder` (at most one stub holds the lock) and every other invariant and property passes. The `stuckx` path, which is accepted, has the same shape (the lock held while the claim is over). Not covered: a redeem write that fails while the claim is still `claimed` and the give-back works (the stub goes back to `stub` and the take is retried; modelling it loops under weak fairness without adding a state the other rows lack). Rows with 4 claims and a revoke, a take-over or a holder crash together ran past 4.7 GB of TLC state files after 25 minutes and were stopped; the 3-claim rows above are the ones recorded.

All rows carry `OneHolder` since the architect's review of #841 (the lockstuck, parkterm, stuckdrop and stucklog cfgs); the state counts above are from before it was added and are re-recorded below.
