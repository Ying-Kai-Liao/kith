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

All rows carry `OneHolder` since the architect's review of #841 (the lockstuck, parkterm, stuckdrop and stucklog cfgs); re-run on 2026-11-05 with `run.sh` (57 rows, every row as expected): the lockstuck rows have the same counts (OneHolder adds no states), `c4-parkterm` is 5,015 states (a queued take-over no longer parks a turn), `c4-stuckdrop` still fails `StuckHasClaim` (71) and `OneHolder` holds on it.

---

# Added by M3-12 (the round-4 gate before the Python turn runner and the claim files go)

Source: `origin/rust-rebuild` 1720d27e, 2026-11-14. `drift.sh` read change by change from the old pin (73a03656), and for `Claims4.tla` from its own source (cf308ebc) and the last model edits (2026-11-05, #841). TLC v1.7.4.

What moved since the models were last touched, and what each did to them:

| Change in the code | Model | Verdict |
|---|---|---|
| K2 (d13b1136): `end_gone` / `recorded_gone` end `ending` only once the pid and the process group it led are gone (`orphan_group`); the daemon's kill re-send (`kill_resend.rs`) and the hello's `end_groups` reach a dead leader's group | `Claims4.tla`: new process state `orphan` (`StubDies`, `OrphanExit`), variants `orphan` (the code) and `pidonly` (the pre-K2 rule) | `c4-orphan` passes (17,792 states); `c4-pidonly` fails `WriterHasClaim` (the claim ends under a writing engine; 1,129 states). Before this the model's `dead` meant "pid and group" and could not tell the two rules apart. |
| Revoke makes an already-`ending` claim's end reason `revoked` so the re-send does not skip it (5b2c548c) | `Revoke` already sets the kill owed on every `ending` claim (`AfterStop` keeps `ending`) | Model was already the rule; the code now follows it. No change. |
| Kill re-send on the daemon's tick, with no holder link (6e1e2f82) | `SendKill` needs the holder up | Not modelled as its own action: it only adds a way for the kill to go out for the host's and NULL-instance claims; a holder's own claims still wait for the link or the next hello (`kill_resend.rs`, "mine"). Safety rows do not depend on it. |
| An exit the core could not record stays in the holder, unacked, and is offered at the next listing (95a3415d) | `Holder.tla` / `Claims4.tla`: exits are kept until a `Report` goes through | Same rule, now also for a failed write. No change. |
| Imported claim files are all `external`; a holder hello never ends an external claim (99473e4b); a hello never ends the host's claims (`HOST_INSTANCE`, ae6f4f23, a02486b6); a holder may not say hello under the host's instance name | `Meet` releases every live claim | The model releases more than the code, so it is the safer side: a claim the code leaves alone stays `running` and `OneWriter` is about processes. Not a separate action. |
| M3-10f: the host's turn claims through the core (`terminal/core_claims.rs`, `hostclaim.rs`, behind `KITH_HOST_CORE_CLAIMS`): the claim file is the writer, the core row its shadow | not modelled | **Gap, on purpose:** the host's writer is the claim file (the thread-writers model covers it) and the core row only mirrors it; every write goes through the same `claim_in` / `report` / `stop` the models cover. The thread-writers skill's re-run after the cutover is the check for the host path (this package's brief leaves it to M3's final check). |
| M3-10c: the wake's turn (`wake/turns.rs`): `start_in` parks `terminal_live`, a run's end closes its waiting turn (`close_gone_in` -> `end_waiting_turn_in`) | `Park` / `parkterm` (c4-parkterm) already; the close only cancels or finishes a waiting turn | The close adds a way for a waiting turn to end, no new writer and no claim: it cannot break a safety row or a liveness one (`P3` gets an extra exit). Not modelled as an action. |
| Stuck-lock claim end keeps the start time read when found stuck (97038a47) | `Stuck` / `lockstuck`: the pid is recorded, then stopped | Same rule; only which start text is recorded changed. |
| Stub: the redeem hook forks nothing (the test `the_redeem_forks_nothing`) | `r4-inherit` (a child that inherits the lock fd wedges the conversation) | The code's answer to that row, now with a test. No model change. |
| `Holder.tla` against `core/holder` | | `core/holder/src/holder.rs` and `proto.rs` have no change since cf308ebc; the rules `Holder.tla` models (high-water, no spawn at or below it, exits kept until acked, per-instance ids) are still at `holder.rs` lines 173, 748-795, 1064-1148. The relay's take/release per turn (M3-4) and its "a weaker answer never replaces a stronger" rule remain outside `Holder.tla`; `WriterLock4.tla`'s one-report-slot lesson (found while running, 1 above) is the same rule at the design level. |

`WriterLock4.tla` models the design (claim ids as current-or-stale, the lock, the holder's group) and no code moved its assumptions: it is unchanged. The two slow rows were re-run for the gate.

**HostHolder (M3 cutover design 2026-11-14, sections 1.1, 1.3, 1.7).** In writer mode the host runs its warm processes under `kith-stub --resident` and plays the holder: a host restart is a holder crash the group survives, with a new instance. `Claims4.tla` gets the variant `hostconst` (one constant host instance: the restarted host reads the old claim as "ours" and reports it done while the old engine still writes under the old stub's lock). `c4-hostholder` (the per-start instance, the old claims stopped to `ending` and killed, `end_gone` decides; the `fix` reading, 378,157 states) passes; `c4-hostconst` fails `WriterHasClaim` (727 states). The design asks for P4; in this module the same fact is `WriterHasClaim` (the claim row ends while a process writes). The warm_procs revoke kill is `Revoke` + `SendKill`/`HKill` (the rows with `MaxRev=1`). Not modelled: the account restart before `take`, and the per-turn take/release of the host's own channel (it is `StubLock` / `RedeemOK` / `GiveBack` / `Stuck` of the `lockstuck` rows).

**Slow rows, 2026-11-14:** `r4-fix-survive-safety` pass (39,444,026 states), `r4-fix-die-safety` pass (22,820,510), the architect's counts.

---

# Added by the re-sync before the claims step (2026-11-17)

Source: `origin/rust-rebuild` f9b5e509 (after #1009, #1010, #1015, #1016, #1017). `drift.sh` read change by change from the old pin
(1720d27e): the M3 core writer (P2 room basis, P5 kernel takers, P6 to P8 the host's Writer mode and readers, P7a/P7b, gaps 1 to 3),
P9 the claims step (#1010), rule K and the host runner (#1015), L-clear (#1016) and the holder bundling. `core/holder/src/holder.rs`
and `proto.rs` have no change since cf308ebc, so `Holder.tla` stands as it was. TLC v1.7.4.

No model action changed: every change below either is a step the models already have, adds a refusal (fewer behaviours, so no
safety row can newly fail and every liveness row keeps its fair actions), or is outside what the three modules model and is listed
as such. Rule K was already added to `Claims4.tla` by #1015 (`CoreRestart`, `CoreKeepsLive`, rows `c4-corekeep`, `c4-corekeepdead`,
`c4-corenokeep`).

| Change in the code | Model | Verdict |
|---|---|---|
| Rule K (#1015, 52a916f0, `conversations/reconcile.rs`, moved out of `claims.rs` with `different_holder`): step 5 keeps the host's `claimed` claims and its `running` ones whose recorded process is alive | `Claims4.tla` `CoreRestart` (#1015) | Modelled. `c4-corekeep` passes, `c4-corekeepdead` passes, `c4-corenokeep` fails `CoreKeepsLive`. |
| Step 5 now runs at the first connected hello of a core generation (gap 1, f07753618, `holder_link.rs` `open_claims`, `claims::is_reconciled`); before it there was no production caller, so claims never opened. A reconnect in the same generation does nothing | `CoreRestart` (one atomic step: restart and reconcile); `Claim` is gated on the reconcile in the code and assumed done in the model | Same rule. The code refuses every claim until that hello, so nothing can claim between the restart and the reconcile, which is what the atomic step says. The wait for a holder is a liveness matter the model gets from `WF(HUp)`. |
| The daemon's tick runs `end_gone` and `warm::settle` (gap 2, `kill_resend.rs` `settle`) | `EndGone` (weakly fair) | The model already lets `EndGone` happen at any time; the tick is the code getting there. No change. |
| B5: each host start names its own instance `kith-serve-host/<pid>-<start>`, `is_host_instance` tells any of them from a holder's; `hostclaim.adopt` (`warm.rs` `adopt`) ends the old instance's claims never redeemed by CAS, moves the rest to `ending` until the process is gone, and marks its warm processes for a kill | `c4-hostholder` (per-start instance) passes; `c4-hostconst` (one constant instance) fails `WriterHasClaim` | `c4-hostholder` is now the code for safety; `c4-hostconst` stays as the broken variant it always was (it was the code at 1720d27e). **Liveness:** the model's `Meet` is weakly fair (it always happens). At the re-sync the host's `adopt_once` (`serve/host/src/agent/bot/writer.rs`) marked itself done before `adopt` answered, so a failed adopt was not retried while that host ran and the old instance's `claimed` and `running` claims stayed until the next host start (`end_gone` ends only `ending` and external claims). Fixed since (flow/adopt-once-retry): `adopt_once` is done only after an adopt that went through; a failed one is tried again by a later Writer-mode begin or slow tick after a wait from 1 s doubling to 60 s (`Adoption`, `adopt_backoff`), one at a time, and a successful one is never asked again (`a_failed_adopt_is_tried_again_and_frees_the_earlier_instances_claims`, `a_successful_adopt_is_never_repeated`). So `Meet` holds as long as the core answers again and a turn comes to the host. Safety was never at stake (the old claims stay writers, nothing claims under them). |
| P7: the idle-warm rule in the core (`external.rs` `blocked_in`: an idle `warm_procs` row of the conversation is marked `kill_at`, the claim answers `waiting` until the group is gone and settled; a process running another conversation's claim is left alone) | `WriterLock4` `r4-killidle` (Q2: kill the idle one first, register only once the group is empty) | `r4-killidle` is now the code. `r4-keepidle` stays the "not vacuous" row. |
| `redeem` refuses (`killing`) a claim on a process marked `kill_at` (the claim stays `claimed`; the caller gives the lock back and stops it with the turn requeued) | `Redeem` / `GiveBack` | A refusal: the caller's path is the give-back plus `stop` the models have (`lockstuck` when the give-back fails). No change. |
| P2 room basis (`basis.rs`, `claims.rs` `fence_in`, `revoke_person`): a turn carries the revoke epochs of every basis person and a room gate; the claim is refused when any moved; a revoke also cancels the turns and stops the claims of turns where the person is a basis person, and kills that person's warm processes (`warm::revoke_in`) | `Claim` (epoch compare), `Revoke`, `SendKill` / `HKill` | One person in the model; the basis is the same compare for more people and the revoke's reach over more rows. Refusals and kills only. `RevokedQuiet` is stated for the one person; it is not checked per basis person. |
| P5 kernel takers (`external.rs` `claim.external`, `claim_terminal`; `terminal.rs` `wire_claim_core`): a terminal or `--direct` run's claim row is `external` (no holder) or, through `terminal.open`, a `terminal` row the holder's stub redeems; a refusal is data (`held`, `takeover`, `waiting`, `revoked`, `agent`); a terminal whose row is not redeemed is ended | `external` claims: outside the model (as before: only `end_gone` and a stop end them); a `terminal` row spawned through the holder is `Claim` + `Spawn` + `Redeem` with no turn | The thread-writers model covers who may take them (`TermClaim`, `TermOpen`, `ExternalChecksWarm`). Here a holder-spawned `terminal` row behaves as a turn's claim on the processes side. Not modelled: a `terminal` row's own kind (`Claims4` has turn and take-over claims only). |
| P8 host readers (`serve/host/src/terminal/core_holders.rs`): Stop and take-over end the claim they read by `hostclaim.stop {claimId}` (a CAS), fail closed when the core does not answer | `AskStop` / `TakeOver` (a stop of the live claim) | A stop by id, as the models' stops. The take-over here stops the terminal's or external claim; `Claims4`'s `TakeOver` (queue a take-over claim, stop the turn's) and `PromoteTW` are still the M3-11 rule, not code. |
| P6/P7b host Writer mode (`core_writer.rs`, `warm_stub.rs`, `writer*.rs`), abandon order (#1009, f8736f59: `stop`, then `killpg`, then `settle_gone`; the lock goes with the stub) | thread-writers `CoreWriter` rows (`AbandonKeepsLock`, `AbandonSettlesEarly`); here `Stop` -> `ending` -> kill -> `EndGone` | The host's side is the thread-writers model's. On the claims side an abandon is a stop of a running claim that ends only once the group is gone, the path `c4-endearly` breaks. No change. |
| #1015 the host runner (`routines/core_runs.rs`, `hostclaim.queued`): the core's queued schedule turns are begun by id (`claim_next`'s CAS) on the core's conversation, locked by the session (`Turn::lock_key`); a usage move keeps the turn (`keep`, no cancel after `exit`) | `Claim` of any queued turn | Same claim steps. The runner's own rules are the schedule-fire skill's `HostRuns.tla`. |
| #1010 the claims step (`cutover/modules/claims.rs`, `import/claims.rs`): with the host stopped and the core's lifetime lock held, every live claim file becomes a `running` `external` row (a proc claim on `proc-<pid>-<start>`; a conversation row made first; a claim that cannot be a row is a blocker that refuses the step). Order: `before_tombstone` writes the takers' tombstone, then re-reads the claim files (`blockers` + `differences`); after it the framework writes the core's marker (`marker::write`) and the `Switched` record (`cutover/mod.rs`). Rollback refuses while a core claim is live | none (the modules start with the claims already in core.db) | **Not modelled.** The import makes only `external` rows, which neither step 5 nor step 7 ends (`reconcile.rs` skips them; `Meet` in the model ends more than the code, the safe side), so the imported writers are outside the modules like any `--direct` run. The switch moment is a store-then-load pair on both sides (the step: tombstone, then re-read the files; `kith-sessions` `take_claim`: link, then read the tombstone again, unlink its own file if switched): at least one sees the other. A refused re-read leaves the tombstone down and the step `stepping`: every taker fails closed until the step is run again or rolled back. Two file takers lack the re-check: the core's `terminal.open` Files path (`terminal.rs`, one `switched` read then `claim::take`) is safe because it runs in the daemon, which holds the lifetime lock the step needs; the Rust twin `kith kernel threads` (`core/kernel/src/threads.rs`) has no tombstone check and is safe only because nothing calls it (`serve/host/src/threads_cmd.rs`). **Condition:** before `kith` is routed to the twin it needs `take_claim`'s link-then-re-check. Checked by reading and by the step's tests (`cutover/modules/claims_tests.rs`), not by a model. |
| #1010 the host's strays sweep (`core_strays.rs`, `hostclaim.strays`): cancels a host-keyed turn that is `queued` or `parked` with no item of this host | the turn rows (`CancelledStays`; a cancel of a queued turn touches no claim) | A cancel of a waiting turn, no claim and no process. It is what makes `P3`'s "a queued turn is claimed" hold for a host turn nobody runs any more; the model's weakly fair `Claim` stands in for a claimer that exists. Not modelled. |
| #1016 L-clear (`core/stub/src/{clear,loaded,main,resident}.rs`): a one-shot on the lock alone, holding the lock and before its engine, kills each warm engine marked as having the conversation loaded (or answers busy, 75); resident stubs keep the `loaded` markers | `WriterLock4` `DLock` (the direct run's take) | It only removes an engine before the direct run starts, under the lock, spawning nothing (`the_clear_forks_nothing`): `DLock` and the rows are unchanged. Modelled in `verification/threads` (`LockAloneClearsWarm`). |
| Holder bundling (`holder_link.rs` `beside_this`, `kith_kernel::helpers::beside`) | none | Which program is found, not what it does. No change. |

`WriterLock4.tla` models the design and nothing above moved its assumptions; its slow rows were re-run for this sync.

**Rows, 2026-11-17** (`run.sh --all`, 10 cores at `nice`): 66 rows, every row as expected, 1553 s. Slow rows: `r4-fix-survive-safety`
pass (39,444,026 states), `r4-fix-die-safety` pass (22,820,510), the same counts as the M3-12 gate.
