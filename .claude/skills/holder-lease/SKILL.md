---
name: holder-lease
description: Model-checks (TLA+/TLC) the holder lease, the rule that a hand cut off from its core kills its guest runs and never an owner run. Use when a change touches the lease code in core/holder/src/holder.rs (Lease, expire_lease, watch_lease, the PING and RENEW arms, the LEASE_LOST answer in spawn, kill_on_lease_loss), core/holder/src/proto.rs (PING, PONG, RENEW, LEASE_LOST, Ping, Pong, Renew, LeaseLost, Exit::lapsed, Welcome::lease_lost), core/holder/src/client.rs (Renewer, answer_pongs, the reader thread's PONG and LEASE_LOST handling, renew), core/holder/src/clock.rs (the lease clock), core/runtime/src/holder_link.rs lease sending (start_renewing, HolderConfig.lease), or a new path that tags a run kill_on_lease_loss. Also use to answer "can a cut-off or sleeping hand keep a revoked guest running?".
---

# Holder lease: a cut-off hand ends its guests, never its owner runs

The core (brain) tags guest and bot runs `kill_on_lease_loss` and keeps a lease alive with RENEW frames. A hand that
hears nothing for a lease period kills those trees itself, so a revoke is never stranded on a cut-off hand
(docs/plans/runtime-design.md section 10). Owner runs are never touched (I18). Whether the deadline can outlive what
the core believes is a question about clocks, sleep, delay and order, so it is checked with TLA+ (TLC), not with tests.

The model is `verification/lease/Lease.tla`. This skill is the only documentation; there is no README.

## Run it

```
verification/lease/run.sh                       # all rows of checks.tsv
verification/lease/run.sh --show VARIANT PROP   # TLC's full output (the counterexample) for one row
verification/lease/drift.sh                     # which covered files changed since the sync commit
```

CI runs `run.sh` (job `holder-lease`) on pull requests and pushes that touch `verification/lease/`, this skill, or the
covered code (the files `drift.sh` lists). `run.sh` exits 0 only when every row of `verification/lease/checks.tsv`
(variant, kind, property, expected pass/fail) came out as expected. Tools are the same pinned, sha256-checked
`tla2tools.jar` v1.7.4 and Temurin JRE as the thread-writers skill (`run.sh` is a copy of its harness), cached under
`${XDG_CACHE_HOME:-$HOME/.cache}/kith-verify/`.

Sizes (constants in `base.consts`): lease `L = 3` ticks, the core stops by tick 3 (`RevokeBy`), at most 2 pings,
one sleep per side. A passing cfg has about 2.4 million states (the liveness row is the slow one); all 25 rows take about 3.5
minutes on a laptop with four parallel workers. A failing cfg stops at its first counterexample. One tick is a third of the real lease (the renewal period).

## What is modelled

Real time `t` (the model's own clock). The hand keeps a clock `clk` that either counts the time the Mac sleeps
(`SleepClockCounts = TRUE`) or stops (FALSE: `Instant`, i.e. CLOCK_MONOTONIC on Linux and mach_absolute_time on macOS, stops in suspend/sleep; the holder's real clock, `clock.rs`, counts it). Either side can sleep once: asleep it runs nothing
and its guest is frozen. Messages are a set in flight, both directions: any order, any delay, lost at will. The
core stops all traffic (revoke, crash, cut off) at some tick. The hand's steps are the code's: every frame is
handled after the expiry check, expiry and the kill are one step, a tagged spawn needs an armed lease else
`LEASE_LOST`, a renewal restores a lost lease, and on waking the first thing is the expiry check.

The core's promise (`belief`): every RENEW it sends says "L from now", so the core stands behind the lease until
the send time + L and no longer. The core needs no clock in the model: a core clock that stalls while the core
sleeps only makes the core wait longer.

| Property | Plain sentence | Harm |
|---|---|---|
| `GuestsWithinBelief` | On an awake hand, once the core's belief has run out, no guest run is alive. A due lapse the 10 ms watcher has not run yet is excluded from the model (`~ExpiryDue`): in code that is the margin the core adds before it releases a guest (see the `belief` row). | A cut-off hand keeps running a revoked guest. |
| `OwnerNeverKilled` | Lease expiry never kills an owner run (I18). | The owner is blocked by the core being down. |
| `NoSilentResurrection` | A renewal read after the lease was lost arms a lease only while the core still stands behind it. | A delayed RENEW revives a lease the core gave up, and a guest spawn after it runs past the core's belief. |
| `RevokedThenDead` (liveness) | Once the core stops renewing, the hand ends with no guest run, for good (weak fairness on time, expiry, waking). | A revoke that never lands. |

## The finding (`today`, fixed by M3-0c)

At rust-rebuild 387c7b5a `holder.rs` failed `GuestsWithinBelief` and `NoSilentResurrection` (`run.sh --show today
GuestsWithinBelief`). Two causes, each enough alone (`read-anchor`, `sleep-stops` rows):

1. **Anchor.** The deadline started when the holder read the RENEW (`Instant::now() + period`), not when the core sent
   it. A RENEW delayed by the link, or sitting in the pipe while the hand's process was stopped, extended the deadline
   past the core's belief; a stale RENEW read after a lapse restored a lease the core no longer stood behind.
2. **Clock.** `Instant` stops while the machine sleeps or suspends (macOS and Linux). A hand that sleeps 10 minutes inside a 30 s lease woke with 29 s
   left, and the revoked guest ran on.

The fix (`fixed` rows pass), now the code: the hand's deadline is its own clock at a moment before the core's send,
plus the period, and the clock counts sleep.

- Ping round: the core sends PING(seq); the hand answers PONG(seq) and remembers its clock for that seq; the core
  answers a PONG with RENEW(seq) (the echo); the hand's deadline is the remembered clock for seq + the period.
  The anchor is a hand event before the core's send, so deadline <= core's send + L whatever the delay, order or loss.
  A renewal whose deadline is already past, or whose seq the hand never answered, arms nothing.
- Without the echo (anchoring at the hand's latest PONG) it breaks again: `no-echo`.

## Code to model

Now (proto `VERSION` 4; holder.rs and friends as of the sync commit below):

| Model action | Code | Notes |
|---|---|---|
| `SendPing`, `CorePong` (core) | `client.rs` `Renewer::ping` / `ping_on` send PING(seq); the reader thread (`over_parts`) answers each PONG with `renew_on` = RENEW(seq, ms) at once, never behind a listing; `holder_link.rs` `start_renewing` pings at once, then every `lease/3` | Seq counts per connection from 1. `Client::renew` is the blocking one-round form (tests) |
| `belief` | not tracked: no run is tagged `kill_on_lease_loss` by the core yet | **Precondition, not code.** When the core starts tagging runs it may release/requeue a hand's guest run only after (send time of the last RENEW + lease) + eps*lease (eps = the hand clock's worst slowness) + the 10 ms watcher + kill latency, or once the hand reports the kill (`Exit::lapsed` on the exit, `Welcome::lease_lost` on reconnect). Written in the doc of `start_renewing` |
| `HandPing` | `holder.rs` `p::PING` arm: stores (seq, `clock()`) in `State::pongs` (newest `PONG_RING` = 8), answers PONG(seq) | The ring is cleared at every hello (`st.pongs.clear()`), so a seq of an older connection anchors nothing (Later L1: done) |
| `HandRenew` | `holder.rs` `p::RENEW` arm: deadline = stored clock for the echoed seq + `ms` (capped at `MAX_LEASE_MS`); unknown seq, `ms = 0` or a deadline `<= clock()` arms nothing | Replaces an armed deadline, also with an earlier one (the model's `lease'`) |
| `Expire`, `WakeHand` | `holder.rs` `expire_lease`: `clock() >= t` -> `Lease::Lost`, then `kill_tree()` for each `kill_on_lease_loss` entry (and marks it `lapsed`), one step under the state lock; `watch_lease` runs it every 10 ms and every frame runs it first | The first run after a wake sees the lapse because the clock counts sleep |
| clock `clk` | `clock.rs` `lease_clock()`: Linux `CLOCK_BOOTTIME`; macOS `CLOCK_MONOTONIC_RAW` (clock_gettime(3): mach_continuous_time, counts sleep; not `CLOCK_UPTIME_RAW`); Windows `QueryInterruptTime` (counts sleep; not `QueryUnbiasedInterruptTime`); other Unix `CLOCK_MONOTONIC` | Tests inject `clock::fake::Fake`. The sleep behaviour itself is from the OS docs: it could not be observed on the machine this was built on (never slept) |
| `HandSpawn` | `holder.rs` spawn: a tagged spawn while the lease is not `Until` -> `LEASE_LOST` frame with `why` (`unarmed` = never armed, `lapsed` = ran out), nothing started | The claim is not used up |
| `CoreLeaseLost` | `client.rs` reader thread: on LEASE_LOST it pings again at once (the frame is still passed on); the retry of the spawn is the caller's | Nothing tags runs yet |
| `OwnerNeverKilled` | `kill_on_lease_loss: false` entries are skipped in `expire_lease` | Unchanged; keep the row |
| `Revoke` (traffic stops) | the link drops or the core stops; the deadline outlives a dropped connection (proto.rs) | Unchanged |
| lapse report | `Exit::lapsed` on the EXITED frame and in listings; `Welcome::lease_lost` | How the core learns the guest runs are gone |

**Decided in M3-0c: a renewal after `LEASE_LOST` restores the lease** (the model's `fixed`, `RenewRestores = TRUE`;
the earlier code did too). It is safe only with the ping anchor and a valid (non-stale, known-seq) renewal from the
newest core. The alternative (`RenewRestores = FALSE`, `no-restore`: the core must re-hello) also passes every safety
row; it stays in the table as the fallback if restore is ever found to hurt. Either way the exits of the killed runs
are kept until acked, which is how the core learns the guest runs are gone.

Not modelled: clock rate drift (a hand clock slow by eps puts the deadline eps*L past the belief; the L/3 renewal cadence protects liveness only, so the core adds eps*L, see the belief row), the 10 ms watcher window,
several holders or cores (generation fencing is the Holder model's job), a holder restart (its leases and runs go
with it), a hand whose processes run on while the machine is asleep (they cannot).

Later: a holder stalled while awake (SIGSTOP, a long GC, a loaded machine) is not modelled: its clock keeps running but nothing acts, so the lapse is late by the stall.

## Broken variants

| Variant | Switch | Fails |
|---|---|---|
| `today` | `Anchor = "read"`, `SleepClockCounts = FALSE` | `GuestsWithinBelief`, `NoSilentResurrection` |
| `read-anchor` | the fix's clock, anchor at read | `GuestsWithinBelief`, `NoSilentResurrection` (delay alone) |
| `sleep-stops` | the fix's anchor, a clock that stops in sleep | `GuestsWithinBelief` (sleep alone) |
| `no-echo` | RENEW without the seq: anchor at the latest PONG | `GuestsWithinBelief` |
| `kill-later` | the kill is a step after the lapse | `GuestsWithinBelief` |
| `kills-owner` | expiry also kills the owner run | `OwnerNeverKilled` |
| `no-restore` | the conservative rule | none (passes) |

The `Never*` rows are expected to fail: they show a guest is spawned and killed, a LEASE_LOST happens, a lost lease is
restored, a hand sleeps with a guest, and a lease lapses while the hand sleeps (`NeverLapsedAsleep`; it passes on
`sleep-stops`, where the clock stops), so a pass is not vacuous.

## When you change covered code

1. `verification/lease/drift.sh`. Nothing listed: no work. Listed files: read each diff and the commit messages.
2. Decide per change: no effect on the model; the model no longer matches (a step split, a check moved, a new
   renewal path, a different clock); or the promise changed on purpose. Update `Lease.tla` and `base.consts` from the
   code, not from what makes a result pass. One step in code = one action; cite the function above the action.
3. `verification/lease/run.sh`. A row that differs is drift (replay the TLC trace against the code) or a bug.
   Since M3-0c the rows `fixed` are the code; `today` is kept as the record of what the old code did.
4. A pass can have lost the ability to fail: break the new check in a scratch edit and see `run.sh` fail.
5. Only then set `SYNC` in `drift.sh` and the sync commit below.

**Sync commit:** `SYNCSHA` (rust-rebuild branch work of M3-0c). The code in the covered files is the `fixed` design.

## Map of the folder

- `Lease.tla`: the model. `base.consts`: the switches for the fix. `variants/*.set`: overrides per variant.
- `checks.tsv`: the table `run.sh` enforces. `run.sh`: pinned tools and the table. `drift.sh`: sync commit and covered files.
