---- MODULE Lease ----
\* The holder lease (docs/plans/runtime-design.md section 10; invariant I18): a hand that cannot reach its brain for a
\* lease period kills its guest runs itself and never touches an owner run. Code today: core/holder/src/holder.rs
\* (Lease, expire_lease, the RENEW arm of the frame loop, the LEASE_LOST answer in spawn), core/holder/src/proto.rs
\* (RENEW, LEASE_LOST), core/runtime/src/holder_link.rs (start_renewing: RENEW right after welcome, then every lease/3).
\*
\* Time. `t` is real time (the model's god clock, one tick = one unit of the lease period L / 3 in the real code).
\* The brain ("core") and the hand ("hand") can each sleep: asleep a side runs nothing, reads nothing, and its guest
\* process is frozen. The hand keeps a clock `clk`. Instant on macOS stops while the Mac sleeps, so the clock of
\* `today` does not advance while the hand sleeps (SleepClockCounts = FALSE); the fix reads a clock that counts sleep
\* (CLOCK_BOOTTIME, mach_continuous_time, interrupt time). The core needs no clock of its own here: its promise is
\* stated in real time (`belief`), and a core clock that stalls in the core's sleep only makes the core wait longer.
\* Clock rate is taken as exact; the real margin for drift is the renewal at L/3 against a deadline at L.
\*
\* The promise. Every RENEW the core sends says "L from now". The core therefore believes the lease can last until
\* the send time + L, and from then on no guest run may still be alive on an awake hand (GuestsWithinBelief). The
\* hand's deadline must never outlive that: messages can be delayed, lost and reordered, and either side can sleep.
\*
\* Anchors (the switch `Anchor` is the whole difference between today and the fix):
\*   "read"   today: the deadline is clk at the moment the hand reads the RENEW, + the period (holder.rs ~484).
\*            A delayed RENEW therefore extends the deadline past the core's belief.
\*   "ping"   the fix: the core sends PING(seq); the hand answers PONG(seq) and remembers its clock at that moment;
\*            the core answers a PONG with RENEW(seq); the hand's deadline is its remembered clock for seq + the
\*            period. The anchor is a hand event before the core's send, so the deadline cannot outlive the belief.
\*   "latest" ping without the echo: RENEW carries no seq, the hand anchors at its latest PONG (a RENEW for an old
\*            ping read after a newer PONG gets the newer anchor).
\* Steps are the code's: the hand expires the lease and kills in one step (expire_lease, under the state lock);
\* every hand frame is handled after the expiry check; a spawn tagged kill_on_lease_loss is answered LEASE_LOST
\* unless the lease is armed; a renewal from the (only, newest) core restores a lost lease.
EXTENDS Naturals, Integers, FiniteSets

CONSTANTS L,                \* the lease period, in ticks
          RevokeBy,         \* the core stops all lease traffic (revoke, crash or cut off) at some t <= RevokeBy
          MaxSeq,           \* pings the core sends in total
          MaxSleeps,        \* sleeps per side
          Anchor,           \* "read" | "ping" | "latest"
          SleepClockCounts, \* TRUE: the hand's clock counts the time it sleeps; FALSE: it stops (Instant on macOS)
          ExpireKills,      \* TRUE: expiry and the kill are one step (the code); FALSE: the kill is a later step
          KillOwner,        \* TRUE: expiry also kills the owner run (a bug; I18)
          RenewRestores     \* TRUE: a renewal restores a lost lease (the code); FALSE: no resurrection, re-hello needed

Max(a, b) == IF a > b THEN a ELSE b
MaxT == RevokeBy + L
Seqs == 1..MaxSeq
NONE == -1

VARIABLES t,          \* real time
          awake,      \* [core |-> BOOLEAN, hand |-> BOOLEAN]
          sleeps,     \* sleeps taken per side
          clk,        \* the hand's clock
          lease,      \* the hand's lease: [st |-> "unarmed" | "until" | "lost", d |-> deadline on clk]
          guest,      \* a guest run (kill_on_lease_loss) is alive on the hand
          owner,      \* an owner run is alive on the hand
          spawned,    \* guest spawns the core has sent
          msgs,       \* messages in flight, both directions, any order
          pongAt,     \* the hand's clock at each PONG it sent (NONE: none)
          lastPong,   \* the hand's clock at its latest PONG
          nseq,       \* pings the core has sent
          belief,     \* the real time until which the core stands behind the lease (exclusive)
          revoked,    \* the core has stopped all lease traffic
          lostSeen,   \* history: the core received a LEASE_LOST
          restored,   \* history: a renewal restored a lost lease
          killedGuest \* history: an expiry killed a live guest

vars == <<t, awake, sleeps, clk, lease, guest, owner, spawned, msgs, pongAt, lastPong, nseq, belief, revoked,
          lostSeen, restored, killedGuest>>

Msg(k, s) == [k |-> k, s |-> s]
LostLease == [st |-> "lost", d |-> 0]

Init ==
  /\ t = 0 /\ awake = [core |-> TRUE, hand |-> TRUE] /\ sleeps = [core |-> 0, hand |-> 0] /\ clk = 0
  /\ lease = [st |-> "unarmed", d |-> 0] /\ guest = FALSE /\ owner = TRUE /\ spawned = 0
  /\ msgs = {} /\ pongAt = [i \in Seqs |-> NONE] /\ lastPong = NONE /\ nseq = 0
  /\ belief = 0 /\ revoked = FALSE /\ lostSeen = FALSE /\ restored = FALSE /\ killedGuest = FALSE

ExpiryDue == lease.st = "until" /\ clk >= lease.d
CoreActive == awake.core /\ ~revoked

\* What the lapse does to the runs (holder.rs expire_lease): the lease is marked lost and every tagged tree is killed
\* in the same step; an owner run (not tagged) is left alone.
LapseGuest == IF ExpireKills THEN FALSE ELSE guest
LapseOwner == IF KillOwner THEN FALSE ELSE owner

\* Time passes. A hand that is awake and has a lapsed lease acts first (expire_lease runs before any frame and every
\* 10 ms): time cannot pass over a due expiry. A sleeping hand does nothing, so its due expiry waits for the wake.
Tick ==
  /\ t < MaxT
  /\ ~(awake.hand /\ ExpiryDue)
  /\ t' = t + 1
  /\ clk' = IF awake.hand \/ SleepClockCounts THEN clk + 1 ELSE clk
  /\ UNCHANGED <<awake, sleeps, lease, guest, owner, spawned, msgs, pongAt, lastPong, nseq, belief, revoked,
                 lostSeen, restored, killedGuest>>

\* holder.rs watch_lease / expire_lease.
Expire ==
  /\ awake.hand /\ ExpiryDue
  /\ lease' = LostLease
  /\ guest' = LapseGuest /\ owner' = LapseOwner
  /\ killedGuest' = (killedGuest \/ (guest /\ ExpireKills))
  /\ UNCHANGED <<t, awake, sleeps, clk, spawned, msgs, pongAt, lastPong, nseq, belief, revoked, lostSeen, restored>>

\* ExpireKills = FALSE only: a kill that comes later than the lapse ("while lost it keeps killing").
KillLost ==
  /\ ~ExpireKills /\ awake.hand /\ lease.st = "lost" /\ guest
  /\ guest' = FALSE /\ killedGuest' = TRUE
  /\ UNCHANGED <<t, awake, sleeps, clk, lease, owner, spawned, msgs, pongAt, lastPong, nseq, belief, revoked,
                 lostSeen, restored>>

SleepSide(s) ==
  /\ awake[s] /\ sleeps[s] < MaxSleeps
  /\ awake' = [awake EXCEPT ![s] = FALSE] /\ sleeps' = [sleeps EXCEPT ![s] = @ + 1]
  /\ UNCHANGED <<t, clk, lease, guest, owner, spawned, msgs, pongAt, lastPong, nseq, belief, revoked,
                 lostSeen, restored, killedGuest>>

WakeCore ==
  /\ ~awake.core /\ awake' = [awake EXCEPT !.core = TRUE]
  /\ UNCHANGED <<t, sleeps, clk, lease, guest, owner, spawned, msgs, pongAt, lastPong, nseq, belief, revoked,
                 lostSeen, restored, killedGuest>>

\* The hand's first thing on waking is the expiry check (the watcher thread and every frame start with it).
WakeHand ==
  /\ ~awake.hand /\ awake' = [awake EXCEPT !.hand = TRUE]
  /\ IF ExpiryDue
       THEN /\ lease' = LostLease /\ guest' = LapseGuest /\ owner' = LapseOwner
            /\ killedGuest' = (killedGuest \/ (guest /\ ExpireKills))
       ELSE UNCHANGED <<lease, guest, owner, killedGuest>>
  /\ UNCHANGED <<t, sleeps, clk, spawned, msgs, pongAt, lastPong, nseq, belief, revoked, lostSeen, restored>>

Lose(m) ==
  /\ m \in msgs /\ msgs' = msgs \ {m}
  /\ UNCHANGED <<t, awake, sleeps, clk, lease, guest, owner, spawned, pongAt, lastPong, nseq, belief, revoked,
                 lostSeen, restored, killedGuest>>

\* ---- the core (holder_link.rs, the brain) ----

\* "ping" and "latest": the core pings, the hand's PONG makes the core send the RENEW.
SendPing ==
  /\ CoreActive /\ Anchor # "read" /\ nseq < MaxSeq
  /\ nseq' = nseq + 1 /\ msgs' = msgs \cup {Msg("PING", nseq + 1)}
  /\ UNCHANGED <<t, awake, sleeps, clk, lease, guest, owner, spawned, pongAt, lastPong, belief, revoked,
                 lostSeen, restored, killedGuest>>

\* start_renewing today: a RENEW on a timer, no answer expected. The core's promise starts here: L from now.
SendRenewDirect ==
  /\ CoreActive /\ Anchor = "read" /\ nseq < MaxSeq
  /\ nseq' = nseq + 1 /\ msgs' = msgs \cup {Msg("RENEW", nseq + 1)}
  /\ belief' = Max(belief, t + L)
  /\ UNCHANGED <<t, awake, sleeps, clk, lease, guest, owner, spawned, pongAt, lastPong, revoked,
                 lostSeen, restored, killedGuest>>

CorePong(m) ==
  /\ m \in msgs /\ m.k = "PONG" /\ CoreActive
  /\ msgs' = (msgs \ {m}) \cup {Msg("RENEW", m.s)}
  /\ belief' = Max(belief, t + L)
  /\ UNCHANGED <<t, awake, sleeps, clk, lease, guest, owner, spawned, pongAt, lastPong, nseq, revoked,
                 lostSeen, restored, killedGuest>>

CoreLeaseLost(m) ==
  /\ m \in msgs /\ m.k = "LOST" /\ awake.core
  /\ msgs' = msgs \ {m} /\ lostSeen' = TRUE
  /\ UNCHANGED <<t, awake, sleeps, clk, lease, guest, owner, spawned, pongAt, lastPong, nseq, belief, revoked,
                 restored, killedGuest>>

\* The core spawns a guest run only while it stands behind the lease.
SendSpawn ==
  /\ CoreActive /\ t < belief /\ spawned < 2
  /\ spawned' = spawned + 1 /\ msgs' = msgs \cup {Msg("SPAWN", spawned + 1)}
  /\ UNCHANGED <<t, awake, sleeps, clk, lease, guest, owner, pongAt, lastPong, nseq, belief, revoked,
                 lostSeen, restored, killedGuest>>

Revoke ==
  /\ ~revoked /\ t <= RevokeBy /\ revoked' = TRUE
  /\ UNCHANGED <<t, awake, sleeps, clk, lease, guest, owner, spawned, msgs, pongAt, lastPong, nseq, belief,
                 lostSeen, restored, killedGuest>>

\* ---- the hand (holder.rs); each frame is handled after expire_lease, so none is read while an expiry is due ----

HandPing(m) ==
  /\ m \in msgs /\ m.k = "PING" /\ awake.hand /\ ~ExpiryDue
  /\ msgs' = (msgs \ {m}) \cup {Msg("PONG", m.s)}
  /\ pongAt' = [pongAt EXCEPT ![m.s] = clk] /\ lastPong' = clk
  /\ UNCHANGED <<t, awake, sleeps, clk, lease, guest, owner, spawned, nseq, belief, revoked,
                 lostSeen, restored, killedGuest>>

AnchorOf(m) == CASE Anchor = "read" -> clk
                 [] Anchor = "ping" -> pongAt[m.s]
                 [] OTHER -> lastPong

\* holder.rs, the RENEW arm. A deadline already past (a stale renewal, read after a wake) arms nothing.
HandRenew(m) ==
  /\ m \in msgs /\ m.k = "RENEW" /\ awake.hand /\ ~ExpiryDue
  /\ msgs' = msgs \ {m}
  /\ LET a == AnchorOf(m)
         d == a + L
         lost == lease.st = "lost"
     IN IF a = NONE \/ d <= clk \/ (lost /\ ~RenewRestores)
          THEN UNCHANGED <<lease, restored>>
          ELSE /\ lease' = [st |-> "until", d |-> d]
               /\ restored' = (restored \/ lost)
  /\ UNCHANGED <<t, awake, sleeps, clk, guest, owner, spawned, pongAt, lastPong, nseq, belief, revoked,
                 lostSeen, killedGuest>>

\* holder.rs spawn: a tagged spawn needs an armed lease, else LEASE_LOST and nothing starts.
HandSpawn(m) ==
  /\ m \in msgs /\ m.k = "SPAWN" /\ awake.hand /\ ~ExpiryDue
  /\ IF lease.st = "until"
       THEN /\ guest' = TRUE /\ msgs' = msgs \ {m}
       ELSE /\ guest' = guest /\ msgs' = (msgs \ {m}) \cup {Msg("LOST", m.s)}
  /\ UNCHANGED <<t, awake, sleeps, clk, lease, owner, spawned, pongAt, lastPong, nseq, belief, revoked,
                 lostSeen, restored, killedGuest>>

Next ==
  \/ Tick \/ Expire \/ KillLost \/ WakeCore \/ WakeHand
  \/ \E s \in {"core", "hand"} : SleepSide(s)
  \/ \E m \in msgs : Lose(m) \/ CorePong(m) \/ CoreLeaseLost(m) \/ HandPing(m) \/ HandRenew(m) \/ HandSpawn(m)
  \/ SendPing \/ SendRenewDirect \/ SendSpawn \/ Revoke

Spec == Init /\ [][Next]_vars /\ WF_vars(Tick) /\ WF_vars(Expire) /\ WF_vars(KillLost) /\ WF_vars(WakeHand)

\* ---- properties ----

TypeOK ==
  /\ t \in 0..MaxT /\ clk \in 0..MaxT /\ lease.st \in {"unarmed", "until", "lost"}
  /\ guest \in BOOLEAN /\ owner \in BOOLEAN /\ spawned \in 0..2 /\ nseq \in 0..MaxSeq /\ belief \in 0..(MaxT + L)

\* Safety: from the moment the core stops standing behind the lease, no guest run is alive on a hand that is running.
\* (A sleeping hand's runs are frozen; its first step on waking is the expiry.)
\* A lapse that is due but not yet run is excluded: the watcher runs it within 10 ms and Tick cannot pass over it,
\* so that window is the real code's margin, not a state of the model.
GuestsWithinBelief == (awake.hand /\ ~ExpiryDue /\ t >= belief) => ~guest

\* I18: expiry never kills an owner run.
OwnerNeverKilled == owner

\* A renewal that arrives after the lease was lost does not arm a lease the core no longer stands behind.
NoSilentResurrection == [][(lease.st = "lost" /\ lease'.st = "until") => t' < belief']_vars

\* Liveness: once the core stops renewing, the hand ends up with no guest run, for good.
RevokedThenDead == revoked ~> []~guest

\* The runs are not vacuous: each of these is expected to FAIL (the thing happens).
NeverSpawned == ~guest
NeverKilled == ~killedGuest
NeverLost == ~lostSeen
NeverRestored == ~restored
NeverHandSleptWithGuest == ~(~awake.hand /\ guest)
====
