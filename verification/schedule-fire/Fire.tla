---- MODULE Fire ----
\* The schedule fire protocol (M4-3). Written by a stand-in (rust-m3-fire-model) for the architect's later review: the
\* architect was blocked on approval cards when M4-3 needed this model, so the stand-in read the design and wrote it.
\* Treat every rule marked "Finding" in the skill (.claude/skills/schedule-fire/SKILL.md) as the stand-in's proposal.
\*
\* The design (architect, 2026-11-04; docs/plans/core-design.md section 9). Job definitions live in <agent>/jobs.json,
\* which the core indexes. Run state is in core.db: sched_tick (the watermark `wm`), sched_fires (unique per job and
\* slot) and the turns of the conversations module (parked turns, deadline: M3-2, m0080_conversations.rs). A fire is ONE
\* transaction: (1) insert into sched_fires, (2) re-check `may` for the job's person, (3) skip if the job's previous turn
\* has not finished (today's kith-sched rule, kernel/bin/kith-sched ~:417), (4) enqueue_turn(source 'schedule', person,
\* deadline = the job's next slot). q218: a scheduled run that finds its conversation held parks as busy and expires at
\* its deadline, so two runs never stack up late. Run now / a webhook use the same `fire` with an ad hoc slot
\* (`manual:<id>`, `event:<id>`). The tick's catch-up: after a missed slot, run once if the miss is under a window.
\*
\* Model action -> planned code (M4-3, core/runtime/src/schedules/; names are the plan's, nothing exists yet):
\*   TickFresh, TickStale   schedules::tick: read the clock, read sched_tick, pick the slot, fire, write sched_tick.
\*                          TickStale is the clock read before the transaction (or a second tick that read it earlier).
\*   AdHocFire              schedules::fire for `manual:<id>` (run now) and `event:<id>` (webhook; payload not modelled)
\*   FireOne                the body of the one transaction (insert fire, re-check person and definition, skip rule,
\*                          enqueue_turn). `Shape` splits it in the two-step variants.
\*   FireB                  the second half of a split fire (broken variants only)
\*   Edit                   the core re-indexing <agent>/jobs.json (pause, unpause, new person): the commit is the index row
\*   Revoke, Grant          the person's access changing in the authority tables (M2); Revoke cancels the person's turns
\*   Claim, Expire, Start,  M3-2's turn rules: claim or park on a held conversation, re-probe, expire at the deadline,
\*   Finish                 spawn, end
\*   Crash, Restart         the core dying and coming back; a claim dies with the core and its turn is queued again
\*   Hold, Release          the conversation held by someone else (a terminal, another run) and let go
\*   Advance                time; one tick of the model is one unit of the clock
\* Modelled time: slots are at P, 2P .. NS*P; the clock `now` runs 0..MaxNow. A job has one person, one definition
\* (paused, person, ver: the index row). Not modelled: cron arithmetic, the payload of a webhook, the 6 h
\* "still running" window of today's code (a turn here is unfinished until it ends or expires), the delivery of a
\* turn's output, generation fencing of the claim (M3-2's model), several jobs (each job is independent: the key is
\* (agent, job, slot)), a stale second core: two ticks racing are two interleaved transactions on one core.db.
EXTENDS Naturals, FiniteSets

CONSTANTS NS,            \* timed slots 1..NS, slot k at time P*k
          P,             \* the clock distance between two slots
          Catch,         \* catch-up window: a missed slot is run only when now - slot < Catch
          MaxEdits,      \* edits of the job (pause, unpause, change person)
          MaxTurns,      \* turn rows available (the model is bounded; a fire with no free row is not enabled)
          AdHoc,         \* ad hoc slots, numbers above NS: 7 = manual:<id> or event:<id> (same code path; may be retried)
          Unique,        \* TRUE: sched_fires is unique per (job, slot) (the design)
          SweepInFire,   \* TRUE: the fire transaction expires turns past their deadline before the skip check
          SkipPrev,      \* TRUE: step 3, skip when the previous turn is unfinished
          ManualSkips,   \* TRUE: run now and webhooks obey the skip rule too
          RecordSkips,   \* TRUE: a skipped fire keeps its sched_fires row with the reason
          MayInTxn,      \* TRUE: the may re-check reads revoke/grant inside the fire transaction
          DefInTxn,      \* TRUE: the job definition (paused, person, ver) is read inside the fire transaction
          PauseCancels,  \* TRUE: pausing cancels the job's queued, parked and claimed turns
          RevokeCancels, \* TRUE: a revoke cancels the person's unfinished turns (M3-2 rule 6)
          DeadlineCheck, \* TRUE: a claim needs now < deadline
          Window,        \* TRUE: the catch-up window applies
          CatchMode,     \* "latest": one fire per tick, the latest due slot | "each": any due slot, one per tick
          Shape,         \* "one-txn" (the design) | "insert-first" | "enqueue-first" (two steps; Crash between)
          TickGuard      \* TRUE (liveness rows): time cannot pass an instant at which no tick ran, nor while the core is down

Persons == {1, 2}
Timed == 1..NS
Slots == Timed \cup AdHoc
Ids == 1..MaxTurns
NONE == 0
MaxNow == P * (NS + 1)
Unfin == {"queued", "claimed", "running", "parked"}
Pending == {"queued", "parked"}
Max(a, b) == IF a > b THEN a ELSE b
SlotTime(s) == IF s \in Timed THEN P * s ELSE 0
NextSlot(x) == (x \div P + 1) * P

NoTurn == [slot |-> NONE, person |-> 0, ver |-> 0, st |-> "none", dl |-> 0]
NoRow == [out |-> "none", n |-> 0, src |-> "none"]
NoPend == [on |-> FALSE, kind |-> "none", id |-> 0, rec |-> NoTurn, slot |-> NONE, row |-> NoRow]

VARIABLES now,      \* the clock
          wm,       \* sched_tick: the watermark, the latest clock any tick has covered
          up,       \* the core is running
          def,      \* the job's index row: [paused, person, ver]
          revoked,  \* [person -> the person's access is revoked]
          held,     \* the job's conversation is held by someone else
          fires,    \* sched_fires: [slot -> [out, n, src]]; out "none" = no row
          turn,     \* turns of the job: [id -> [slot, person, ver, st, dl]]
          pend,     \* a split fire between its two steps (broken variants)
          defHist,  \* every definition the index has had (read only when DefInTxn is FALSE)
          revHist   \* every revoke map the authority has had (read only when MayInTxn is FALSE)

vars == <<now, wm, up, def, revoked, held, fires, turn, pend, defHist, revHist>>
env == <<now, up, def, revoked, held, defHist, revHist>>

Def0 == [paused |-> FALSE, person |-> 1, ver |-> 0]
Rev0 == [p \in Persons |-> FALSE]

Init ==
  /\ now = 0 /\ wm = 0 /\ up = TRUE /\ def = Def0 /\ revoked = Rev0 /\ held = FALSE
  /\ fires = [s \in Slots |-> NoRow] /\ turn = [i \in Ids |-> NoTurn] /\ pend = NoPend
  /\ defHist = {Def0} /\ revHist = {Rev0}

\* ---- the fire transaction ----------------------------------------------------------------------------------------

RowOf(o, n, src) == [out |-> o, n |-> n, src |-> src]
\* The deadline is the first slot after the later of the slot and the read clock, so a caught-up fire still has a
\* future deadline (Finding 2 in the skill).
NewTurn(s, n, d) == [slot |-> s, person |-> d.person, ver |-> d.ver, st |-> "queued",
                     dl |-> NextSlot(Max(n, SlotTime(s)))]
SkipOn(s) == SkipPrev /\ (s \in Timed \/ ManualSkips)

Swept(n) == [i \in Ids |-> IF SweepInFire /\ turn[i].st \in Pending /\ turn[i].dl <= n
                           THEN [turn[i] EXCEPT !.st = "expired"] ELSE turn[i]]

\* One fire for slot s, reading the clock n, the definition d and the revoke map r. Constrains fires', turn', pend'.
FireOne(s, n, d, r, src) ==
  LET sw == Swept(n)
      unfinished == {i \in Ids : sw[i].st \in Unfin}
      o == IF d.paused THEN "skip_paused"
           ELSE IF r[d.person] THEN "skip_revoked"
           ELSE IF SkipOn(s) /\ unfinished # {} THEN "skip_prev"
           ELSE "fired"
      free == {i \in Ids : sw[i].st = "none"}
      id == CHOOSE i \in free : \A j \in free : i <= j
  IN IF Unique /\ fires[s].out # "none"
       THEN UNCHANGED <<fires, turn, pend>>           \* the insert conflicts: nothing happens
     ELSE IF o # "fired"
       THEN /\ fires' = IF RecordSkips THEN [fires EXCEPT ![s] = RowOf(o, n, src)] ELSE fires
            /\ turn' = sw /\ pend' = NoPend
     ELSE /\ free # {}
          /\ IF Shape = "one-txn"
               THEN /\ fires' = [fires EXCEPT ![s] = RowOf("fired", n, src)]
                    /\ turn' = [sw EXCEPT ![id] = NewTurn(s, n, d)] /\ pend' = NoPend
             ELSE IF Shape = "insert-first"
               THEN /\ fires' = [fires EXCEPT ![s] = RowOf("fired", n, src)] /\ turn' = sw
                    /\ pend' = [on |-> TRUE, kind |-> "turn", id |-> 0, rec |-> NewTurn(s, n, d), slot |-> s,
                                row |-> NoRow]
             ELSE /\ fires' = fires /\ turn' = [sw EXCEPT ![id] = NewTurn(s, n, d)]
                  /\ pend' = [on |-> TRUE, kind |-> "row", id |-> 0, rec |-> NoTurn, slot |-> s,
                              row |-> RowOf("fired", n, src)]

\* Second half of a split fire.
FireB ==
  /\ up /\ pend.on
  /\ IF pend.kind = "turn"
       THEN \E i \in Ids : turn[i].st = "none" /\ turn' = [turn EXCEPT ![i] = pend.rec] /\ fires' = fires
       ELSE fires' = [fires EXCEPT ![pend.slot] = pend.row] /\ turn' = turn
  /\ pend' = NoPend
  /\ UNCHANGED <<wm, env>>

DefChoices == IF DefInTxn THEN {def} ELSE defHist
RevChoices == IF MayInTxn THEN {revoked} ELSE revHist

\* The tick: the clock read is n (n < now when the read was taken before the transaction). The watermark is read in the
\* transaction. Catch-up: the due slots are those after the watermark, up to n, inside the window.
TickAt(n) ==
  /\ up /\ ~pend.on
  /\ \E d \in DefChoices, r \in RevChoices :
       LET due == {k \in Timed : wm < P * k /\ P * k <= n /\ (~Window \/ n - P * k < Catch)}
       IN IF due = {}
            THEN /\ UNCHANGED <<fires, turn, pend>> /\ wm' = Max(wm, n)
            ELSE \E k \in (IF CatchMode = "latest" THEN {CHOOSE m \in due : \A j \in due : j <= m} ELSE due) :
                   /\ FireOne(k, n, d, r, "tick")
                   /\ wm' = IF CatchMode = "latest" THEN Max(wm, n) ELSE Max(wm, P * k)
  /\ UNCHANGED env

TickFresh == TickAt(now)
TickStale == now > 0 /\ TickAt(now - 1)   \* one unit stale is enough to race; a wider range only adds states

AdHocFire ==
  /\ up /\ ~pend.on /\ now < MaxNow
  /\ \E s \in AdHoc, d \in DefChoices, r \in RevChoices : FireOne(s, now, d, r, "adhoc")
  /\ UNCHANGED <<wm, env>>

\* ---- the owner, the authority and the core ------------------------------------------------------------------------

Edit ==
  /\ up /\ def.ver < MaxEdits
  /\ \E p \in Persons, b \in BOOLEAN :
       LET d2 == [paused |-> b, person |-> p, ver |-> def.ver + 1]
       IN /\ def' = d2
          /\ defHist' = IF DefInTxn THEN defHist ELSE defHist \cup {d2}
          /\ turn' = IF PauseCancels /\ b
                       THEN [i \in Ids |-> IF turn[i].st \in Pending \cup {"claimed"}
                                           THEN [turn[i] EXCEPT !.st = "cancelled"] ELSE turn[i]]
                       ELSE turn
  /\ UNCHANGED <<now, wm, up, revoked, held, fires, pend, revHist>>

Revoke(p) ==
  /\ up /\ ~revoked[p]
  /\ LET r2 == [revoked EXCEPT ![p] = TRUE]
     IN /\ revoked' = r2
        /\ revHist' = IF MayInTxn THEN revHist ELSE revHist \cup {r2}
  /\ turn' = IF RevokeCancels
               THEN [i \in Ids |-> IF turn[i].person = p /\ turn[i].st \in Unfin
                                   THEN [turn[i] EXCEPT !.st = "cancelled"] ELSE turn[i]]
               ELSE turn
  /\ UNCHANGED <<now, wm, up, def, held, fires, pend, defHist>>

Grant(p) ==
  /\ up /\ revoked[p]
  /\ LET r2 == [revoked EXCEPT ![p] = FALSE]
     IN /\ revoked' = r2
        /\ revHist' = IF MayInTxn THEN revHist ELSE revHist \cup {r2}
  /\ UNCHANGED <<now, wm, up, def, held, fires, turn, pend, defHist>>

Crash ==
  /\ up /\ up' = FALSE /\ pend' = NoPend
  /\ turn' = [i \in Ids |-> IF turn[i].st = "claimed" THEN [turn[i] EXCEPT !.st = "queued"] ELSE turn[i]]
  /\ UNCHANGED <<now, wm, def, revoked, held, fires, defHist, revHist>>

Restart ==
  /\ ~up /\ up' = TRUE
  /\ UNCHANGED <<now, wm, def, revoked, held, fires, turn, pend, defHist, revHist>>

Hold == /\ ~held /\ held' = TRUE /\ UNCHANGED <<now, wm, up, def, revoked, fires, turn, pend, defHist, revHist>>
Release == /\ held /\ held' = FALSE /\ UNCHANGED <<now, wm, up, def, revoked, fires, turn, pend, defHist, revHist>>

Advance ==
  /\ now < MaxNow
  /\ TickGuard => (up /\ wm >= now)
  /\ now' = now + 1
  /\ UNCHANGED <<wm, up, def, revoked, held, fires, turn, pend, defHist, revHist>>

\* ---- the turn lifecycle (M3-2: queued -> claimed -> running -> done, parked, expired) ------------------------------

TurnStep(i, new) ==
  /\ turn' = [turn EXCEPT ![i].st = new]
  /\ UNCHANGED <<now, wm, up, def, revoked, held, fires, pend, defHist, revHist>>

\* A queued turn tries the conversation: held -> parked (lock_busy); free -> claimed. A parked turn is re-probed.
Claim(i) ==
  /\ up /\ turn[i].st \in Pending
  /\ (~DeadlineCheck \/ now < turn[i].dl)
  /\ IF held THEN turn[i].st = "queued" /\ TurnStep(i, "parked") ELSE TurnStep(i, "claimed")

Expire(i) == /\ up /\ turn[i].st \in Pending /\ turn[i].dl <= now /\ TurnStep(i, "expired")
Start(i) == /\ up /\ turn[i].st = "claimed" /\ TurnStep(i, "running")
Finish(i) == /\ up /\ turn[i].st = "running" /\ TurnStep(i, "done")

ClaimAny == \E i \in Ids : Claim(i)
ExpireAny == \E i \in Ids : Expire(i)
StartAny == \E i \in Ids : Start(i)
FinishAny == \E i \in Ids : Finish(i)

Next ==
  \/ TickFresh \/ TickStale \/ AdHocFire \/ FireB
  \/ Edit \/ (\E p \in Persons : Revoke(p) \/ Grant(p))
  \/ Crash \/ Restart \/ Hold \/ Release \/ Advance
  \/ ClaimAny \/ ExpireAny \/ StartAny \/ FinishAny

Fairness ==
  /\ WF_vars(Advance) /\ WF_vars(TickFresh) /\ WF_vars(Restart) /\ WF_vars(FireB)
  /\ WF_vars(ClaimAny) /\ WF_vars(ExpireAny) /\ WF_vars(StartAny) /\ WF_vars(FinishAny)
  \* The host crashes finitely often. Without this TLC finds claim -> crash -> restart -> claim forever, a turn that
  \* never gets to run: that is a crash loop (an operations problem), not a flaw in the fire protocol.
  /\ <>[][~Crash]_vars

Spec == Init /\ [][Next]_vars /\ Fairness

\* ---- properties --------------------------------------------------------------------------------------------------

TypeOK ==
  /\ now \in 0..MaxNow /\ wm \in 0..MaxNow /\ up \in BOOLEAN /\ held \in BOOLEAN
  /\ def.ver \in 0..MaxEdits /\ revoked \in [Persons -> BOOLEAN]
  /\ \A s \in Slots : fires[s].out \in {"none", "fired", "skip_prev", "skip_paused", "skip_revoked"}
  /\ \A i \in Ids : turn[i].st \in {"none", "queued", "claimed", "running", "parked", "done", "expired", "cancelled"}

TurnsOf(s) == {i \in Ids : turn[i].slot = s}

\* A slot fires at most once: one turn per slot. (Run now's `manual:<id>` and a webhook's `event:<id>` are slots too.)
AtMostOncePerSlot == \A s \in Slots : Cardinality(TurnsOf(s)) <= 1

\* At most one unfinished turn per job.
NoStacking == Cardinality({i \in Ids : turn[i].st \in Unfin}) <= 1

\* A fire row and its turn exist together: a crash between the insert and the enqueue cannot lose or double a fire.
\* (Outside a split fire's pending step, which only the two-step variants have.)
CrashSafe ==
  /\ \A s \in Slots : fires[s].out = "fired" => TurnsOf(s) # {} \/ (pend.on /\ pend.slot = s)
  /\ \A i \in Ids : turn[i].st # "none" => fires[turn[i].slot].out # "none" \/ (pend.on /\ pend.slot = turn[i].slot)

\* After a revoke commits, nothing of the person is queued, parked, claimed or running.
NoRunForRevoked == \A i \in Ids : turn[i].st # "none" => (revoked[turn[i].person] => turn[i].st \notin Unfin)

\* A catch-up never runs a slot older than the window.
CatchUpWindow == \A s \in Timed : fires[s].src = "tick" => fires[s].n - P * s < Catch

\* One fire per missed span: no two fires of one tick cover (the same clock reading, different slots).
CatchUpOnce ==
  \A s1, s2 \in Timed : (s1 # s2 /\ fires[s1].out = "fired" /\ fires[s2].out = "fired" /\ fires[s1].src = "tick"
                         /\ fires[s2].src = "tick") => fires[s1].n # fires[s2].n

\* Action properties: what a step may create or start.
NoFireForRevokedPerson ==
  [][\A i \in Ids : (turn[i].st = "none" /\ turn'[i].st # "none") => ~revoked'[turn'[i].person]]_vars
PausedNeverFires ==
  [][\A i \in Ids : (turn[i].st = "none" /\ turn'[i].st # "none") => ~def'.paused]_vars
EditUsesNewDefinition ==
  [][\A i \in Ids : (turn[i].st = "none" /\ turn'[i].st # "none")
        => (turn'[i].ver = def'.ver /\ turn'[i].person = def'.person)]_vars
\* The claim is the start: a turn is claimed only before its deadline.
DeadlineExpires ==
  [][\A i \in Ids : (turn[i].st # "claimed" /\ turn'[i].st = "claimed") => now' < turn'[i].dl]_vars
\* After a pause commits, no turn starts (claims) until the job is unpaused.
PausedNeverStarts ==
  [][\A i \in Ids : (turn[i].st # "claimed" /\ turn'[i].st = "claimed") => ~def'.paused]_vars
\* A skip is recorded only when a live turn exists: not a parked turn past its deadline that no sweep has reached.
SkipIsJustified ==
  [][\A s \in Slots : (fires[s].out # "skip_prev" /\ fires'[s].out = "skip_prev")
       => \E i \in Ids : turn[i].st \in Unfin /\ ~(turn[i].st \in Pending /\ turn[i].dl <= fires'[s].n)]_vars

\* Liveness (rows with TickGuard): the last slot gets a row (fired or skipped with its reason) once its time is over;
\* and a turn whose deadline is inside the model ends.
DueGetsRow == (now = MaxNow) ~> (fires[NS].out # "none")
TurnsEnd == \A i \in Ids : ((turn[i].st \in Unfin /\ turn[i].dl <= MaxNow) ~> (turn[i].st \notin Unfin))

\* Non-vacuity: each of these happens (the invariant FAILS).
NeverFired == \A s \in Slots : fires[s].out # "fired"
NeverSkipPrev == \A s \in Slots : fires[s].out # "skip_prev"
NeverSkipPaused == \A s \in Slots : fires[s].out # "skip_paused"
NeverSkipRevoked == \A s \in Slots : fires[s].out # "skip_revoked"
NeverAdHocFired == \A s \in AdHoc : fires[s].out # "fired"
NeverCaughtUp == \A s \in Timed : ~(fires[s].out = "fired" /\ fires[s].n > P * s)
NeverParked == \A i \in Ids : turn[i].st # "parked"
NeverExpired == \A i \in Ids : turn[i].st # "expired"
NeverDone == \A i \in Ids : turn[i].st # "done"
NeverCancelled == \A i \in Ids : turn[i].st # "cancelled"
NeverDown == up
====
