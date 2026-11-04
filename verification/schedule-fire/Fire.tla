---- MODULE Fire ----
\* The schedule fire protocol (M4-3). Written by a stand-in (rust-m3-fire-model), then reviewed by the architect (PR #751,
\* head ffb3bf3c: "approve after the MUSTs"); this version applies that review. The rules the skill
\* (.claude/skills/schedule-fire/SKILL.md) lists as "M4-3 requirements" are the architect's list, each naming the row
\* that fails without it.
\*
\* The design (architect, 2026-11-04; docs/plans/core-design.md section 9). Job definitions live in <agent>/jobs.json,
\* which the core indexes. Run state is in core.db: sched_tick (the watermark `wm`), sched_fires (unique per job and
\* slot) and the turns of the conversations module (parked turns, deadline: M3-2, m0080_conversations.rs). A fire is ONE
\* transaction: (1) insert into sched_fires, (2) re-check `may` for the job's person, (3) skip if the job's previous turn
\* has not finished (today's kith-sched rule, kernel/bin/kith-sched ~:417), (4) enqueue_turn(source 'schedule', person,
\* deadline = the job's next slot). q218: a scheduled run that finds its conversation held parks as busy and expires at
\* its deadline, so two runs never stack up late. Run now / a webhook use the same `fire` with an ad hoc slot
\* (`manual:<id>`, `event:<id>`), but a BUSY ad hoc fire writes no key and tells its caller (busy / retry-later); a retry
\* of a decided id answers the recorded outcome. The tick's catch-up: after a missed slot, run once if the miss is under
\* a window; every slot the watermark passes without a fire gets a `skipped:missed` row.
\*
\* Model action -> code (M4-3 is not written yet; the M3-2 names are on rust-rebuild, core/runtime/src/conversations/claims.rs):
\*   TickFresh, TickStale   schedules::tick: read the clock, read sched_tick, pick the slot, fire, write sched_tick.
\*                          TickStale is the clock read before the transaction (or a second tick that read it earlier).
\*   AdHocFire              schedules::fire for `manual:<id>` (run now) and `event:<id>` (webhook; payload not modelled),
\*                          and the answer to the caller (`ans`): busy, a recorded outcome, or the new outcome
\*   FireOne                the body of the one transaction (insert fire, re-check person and definition, skip rule,
\*                          enqueue_turn). `Shape` splits it in the two-step variants; `TickFirst` saves the watermark first.
\*   FireB                  the second half of a split fire (broken variants only)
\*   Edit, CancelPaused     the core re-indexing <agent>/jobs.json (pause, unpause, new person): the commit is the index
\*                          row; a pause cancels the job's turns in the same transaction (`PauseLater`: a second one)
\*   RevokeAuth, Grant      the person's access changing in the authority tables (M2)
\*   RevokePerson           claims.rs revoke_person: cancels the person's unfinished turns and bumps revoke_epoch, AFTER the authority revoke
\*   Claim, Expire, Start,  M3-2's turn rules: claim (with the may check) or park on a held conversation, re-probe,
\*   Finish                 expire at the deadline, spawn, end
\*   Crash, Restart         the core dying and coming back; a claim dies with the core and its turn is queued again
\*   Hold, Release          the conversation held by someone else (a terminal, another run) and let go
\*   Advance                time; one tick of the model is one unit of the clock, and it runs while the core is down
\* Modelled time: slots are at P, 2P .. NS*P; the clock `now` runs 0..MaxNow. A job has one person, one definition
\* (paused, person, ver: the index row). Not modelled: cron arithmetic, the payload of a webhook, the 6 h
\* "still running" window of today's code (a turn here is unfinished until it ends or expires), the delivery of a
\* turn's output, generation fencing of the claim (M3-2's model), several jobs (each job is independent: the key is
\* (agent, job, slot)), a stale second core: two ticks racing are two interleaved transactions on one core.db, and the
\* run-now / webhook TTL (the deadline of an ad hoc fire is the next slot after the clock only).
EXTENDS Naturals, FiniteSets

CONSTANTS NS,            \* timed slots 1..NS, slot k at time P*k
          P,             \* the clock distance between two slots
          Catch,         \* catch-up window: a missed slot is run only when now - slot < Catch
          MaxEdits,      \* edits of the job (pause, unpause, change person)
          MaxRevs,       \* authority revokes per person (0: no revoke or grant at all)
          MaxTurns,      \* turn rows available (the model is bounded; a fire with no free row is not enabled)
          AdHoc,         \* ad hoc slots, numbers above NS: 7 = manual:<id> or event:<id> (same code path; may be retried)
          Unique,        \* TRUE: sched_fires is unique per (job, slot) (the design)
          SweepInFire,   \* TRUE: the fire transaction expires turns past their deadline before the skip check
          SkipPrev,      \* TRUE: step 3, skip when the previous turn is unfinished
          ManualSkips,   \* TRUE: run now and webhooks obey the skip rule too
          BusyTakesNoKey,\* TRUE: a busy run-now / webhook writes no sched_fires row and answers busy (retry-later)
          RecordSkips,   \* TRUE: a skipped fire keeps its sched_fires row with the reason
          MissedRows,    \* TRUE: a slot the watermark passes without a fire gets a skipped:missed row (requirement j)
          MayInTxn,      \* TRUE: the may check reads the authority inside the fire transaction (one database)
          EpochCheck,    \* TRUE: the fire carries revoke_seen and refuses (skipped:revoked) if a revoke committed since
          DefInTxn,      \* TRUE: the job definition (paused, person, ver) is read inside the fire transaction
          PauseCancels,  \* TRUE: pausing cancels the job's queued, parked and claimed turns
          PauseLater,    \* TRUE: that cancel is a second transaction after the index write (needs PauseCancels)
          RevokeCancels, \* TRUE: revoke_person cancels the person's unfinished turns (M3-2 rule 6)
          ClaimMay,      \* TRUE: a claim carries a may check and revoke_seen (claims.rs:32-37); FALSE: no may check at all
          DeadlineCheck, \* TRUE: a claim needs now < deadline
          DlFromSlot,    \* TRUE (broken): the deadline is the next slot after the SLOT time, not after the clock read
          Window,        \* TRUE: the catch-up window applies
          CatchMode,     \* "latest": one fire per tick, the latest due slot | "each": any due slot, one per tick
          Shape,         \* "one-txn" (the design) | "insert-first" | "enqueue-first" (two steps; Crash between)
          TickFirst,     \* TRUE (broken): the tick saves the watermark in one transaction and fires in the next
          Holds,         \* TRUE: the conversation can be held by someone else
          AdHocLate,     \* TRUE: an ad hoc fire is possible at the last instant too (the webhook liveness row)
          Redeliver,     \* TRUE: an unaccepted ad hoc delivery is retried until accepted (strong fairness)
          TickGuard      \* TRUE (liveness rows): time cannot pass an instant at which no tick ran while the core is up

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
NoPend == [on |-> FALSE, kind |-> "none", id |-> 0, rec |-> NoTurn, slot |-> NONE, row |-> NoRow, n |-> 0]

VARIABLES now,      \* the clock
          wm,       \* sched_tick: the watermark, the latest clock any tick has covered
          up,       \* the core is running
          def,      \* the job's index row: [paused, person, ver]
          revoked,  \* [person -> the authority has revoked the person]
          repoch,   \* [person -> revoke_epoch: bumped by revoke_person]
          cut,      \* [person -> revoke_person has run for the current revoke]
          held,     \* the job's conversation is held by someone else
          fires,    \* sched_fires: [slot -> [out, n, src]]; out "none" = no row
          turn,     \* turns of the job: [id -> [slot, person, ver, st, dl]]
          ans,      \* [slot -> what the ad hoc caller was last told: none, busy, or the recorded outcome]
          pend,     \* a split fire between its two steps (broken variants)
          defHist,  \* every definition the index has had (read only when DefInTxn is FALSE)
          revHist   \* every (revoked, repoch) the authority has had (read only when MayInTxn is FALSE)

vars == <<now, wm, up, def, revoked, repoch, cut, held, fires, turn, ans, pend, defHist, revHist>>
env == <<now, up, def, revoked, repoch, cut, held, defHist, revHist>>

Def0 == [paused |-> FALSE, person |-> 1, ver |-> 0]
Rev0 == [p \in Persons |-> FALSE]
Ep0 == [p \in Persons |-> 0]
Auth0 == [rv |-> Rev0, ep |-> Ep0]

Init ==
  /\ now = 0 /\ wm = 0 /\ up = TRUE /\ def = Def0 /\ held = FALSE
  /\ revoked = Rev0 /\ repoch = Ep0 /\ cut = Rev0
  /\ fires = [s \in Slots |-> NoRow] /\ turn = [i \in Ids |-> NoTurn] /\ pend = NoPend
  /\ ans = [s \in Slots |-> "none"]
  /\ defHist = {Def0} /\ revHist = {Auth0}

\* ---- the fire transaction ----------------------------------------------------------------------------------------

DefChoices == IF DefInTxn THEN {def} ELSE defHist
AuthChoices == IF MayInTxn THEN {[rv |-> revoked, ep |-> repoch]} ELSE revHist

RowOf(o, n, src) == [out |-> o, n |-> n, src |-> src]
\* The deadline is the first slot after the later of the slot and the read clock, so a caught-up fire, a run now and a
\* webhook (slot time 0) still have a future deadline (FiredIsLive; requirement 6).
NewTurn(s, n, d) == [slot |-> s, person |-> d.person, ver |-> d.ver, st |-> "queued",
                     dl |-> IF DlFromSlot THEN NextSlot(SlotTime(s)) ELSE NextSlot(Max(n, SlotTime(s)))]
SkipOn(s) == SkipPrev /\ (s \in Timed \/ ManualSkips)

Swept(n) == [i \in Ids |-> IF SweepInFire /\ turn[i].st \in Pending /\ turn[i].dl <= n
                           THEN [turn[i] EXCEPT !.st = "expired"] ELSE turn[i]]

\* What a fire for slot s decides, reading the clock n, the definition d and the authority a = [rv, ep].
Decision(s, n, d, a) ==
  LET sw == Swept(n)
      unfinished == {i \in Ids : sw[i].st \in Unfin}
  IN IF d.paused THEN "skip_paused"
     ELSE IF a.rv[d.person] \/ (EpochCheck /\ a.ep[d.person] # repoch[d.person]) THEN "skip_revoked"
     ELSE IF SkipOn(s) /\ unfinished # {} THEN "skip_prev"
     ELSE "fired"

\* One fire for slot s. A record of the new fires, turn and pend; ok = the step is enabled.
FireRes(s, n, d, a, src) ==
  LET sw == Swept(n)
      o == Decision(s, n, d, a)
      free == {i \in Ids : sw[i].st = "none"}
      id == CHOOSE i \in free : \A j \in free : i <= j
      same == [ok |-> TRUE, fires |-> fires, turn |-> turn, pend |-> pend]
  IN IF Unique /\ fires[s].out # "none"
       THEN same                                       \* the insert conflicts: nothing happens
     ELSE IF o = "skip_prev" /\ src = "adhoc" /\ BusyTakesNoKey
       THEN [same EXCEPT !.turn = sw]                  \* busy: no key, the caller is told
     ELSE IF o # "fired"
       THEN [ok |-> TRUE, fires |-> IF RecordSkips THEN [fires EXCEPT ![s] = RowOf(o, n, src)] ELSE fires,
             turn |-> sw, pend |-> NoPend]
     ELSE IF free = {}
       THEN [same EXCEPT !.ok = FALSE]
     ELSE IF Shape = "one-txn"
       THEN [ok |-> TRUE, fires |-> [fires EXCEPT ![s] = RowOf("fired", n, src)],
             turn |-> [sw EXCEPT ![id] = NewTurn(s, n, d)], pend |-> NoPend]
     ELSE IF Shape = "insert-first"
       THEN [ok |-> TRUE, fires |-> [fires EXCEPT ![s] = RowOf("fired", n, src)], turn |-> sw,
             pend |-> [NoPend EXCEPT !.on = TRUE, !.kind = "turn", !.rec = NewTurn(s, n, d), !.slot = s]]
     ELSE [ok |-> TRUE, fires |-> fires, turn |-> [sw EXCEPT ![id] = NewTurn(s, n, d)],
           pend |-> [NoPend EXCEPT !.on = TRUE, !.kind = "row", !.slot = s, !.row = RowOf("fired", n, src)]]

FireOne(s, n, d, a, src) ==
  LET R == FireRes(s, n, d, a, src)
  IN R.ok /\ fires' = R.fires /\ turn' = R.turn /\ pend' = R.pend

\* Second half of a split fire: the other write of insert-first / enqueue-first, or the fire after tick-first's watermark.
FireB ==
  /\ up /\ pend.on
  /\ IF pend.kind = "fire"
       THEN \E d \in DefChoices, a \in AuthChoices : FireOne(pend.slot, pend.n, d, a, "tick")
       ELSE /\ IF pend.kind = "turn"
                 THEN \E i \in Ids : turn[i].st = "none" /\ turn' = [turn EXCEPT ![i] = pend.rec] /\ fires' = fires
                 ELSE fires' = [fires EXCEPT ![pend.slot] = pend.row] /\ turn' = turn
            /\ pend' = NoPend
  /\ UNCHANGED <<wm, ans, env>>

\* Rows for the slots the watermark passes (lo, hi] without a fire: skipped:missed (requirement j).
WithMissed(f, lo, hi, fired, n) ==
  [j \in Slots |-> IF MissedRows /\ j \in Timed /\ j # fired /\ lo < P * j /\ P * j <= hi /\ f[j].out = "none"
                   THEN RowOf("skip_missed", n, "tick") ELSE f[j]]

\* The tick: the clock read is n (n < now when the read was taken before the transaction). The watermark is read in the
\* transaction. Catch-up: the due slots are those after the watermark, up to n, inside the window.
TickAt(n) ==
  /\ up /\ ~pend.on
  /\ \E d \in DefChoices, a \in AuthChoices :
       LET due == {k \in Timed : wm < P * k /\ P * k <= n /\ (~Window \/ n - P * k < Catch)}
       IN IF due = {}
            THEN /\ wm' = Max(wm, n) /\ fires' = WithMissed(fires, wm, Max(wm, n), NONE, n)
                 /\ UNCHANGED <<turn, pend>>
            ELSE \E k \in (IF CatchMode = "latest" THEN {CHOOSE m \in due : \A j \in due : j <= m} ELSE due) :
                   LET nw == IF CatchMode = "latest" THEN Max(wm, n) ELSE Max(wm, P * k)
                       R == FireRes(k, n, d, a, "tick")
                   IN /\ wm' = nw
                      /\ IF TickFirst
                           THEN /\ fires' = WithMissed(fires, wm, nw, k, n) /\ turn' = turn
                                /\ pend' = [NoPend EXCEPT !.on = TRUE, !.kind = "fire", !.slot = k, !.n = n]
                           ELSE /\ R.ok /\ fires' = WithMissed(R.fires, wm, nw, k, n)
                                /\ turn' = R.turn /\ pend' = R.pend
  /\ UNCHANGED <<ans, env>>

TickFresh == TickAt(now)
TickStale == now > 0 /\ TickAt(now - 1)   \* one unit stale is enough to race; a wider range only adds states

\* Run now / a webhook delivery (or its retry). The caller is told: a recorded outcome for a decided id, busy (retry-later)
\* when the previous run is unfinished, else the outcome the fire just wrote.
AdHocFire ==
  /\ up /\ ~pend.on /\ (AdHocLate \/ now < MaxNow)
  /\ \E s \in AdHoc, d \in DefChoices, a \in AuthChoices :
       /\ FireOne(s, now, d, a, "adhoc")
       /\ ans' = [ans EXCEPT ![s] = IF Unique /\ fires[s].out # "none" THEN fires[s].out
                                    ELSE IF Decision(s, now, d, a) = "skip_prev" THEN "busy"
                                    ELSE Decision(s, now, d, a)]
  /\ UNCHANGED <<wm, env>>

\* ---- the owner, the authority and the core ------------------------------------------------------------------------

CancelAll(st) == [i \in Ids |-> IF turn[i].st \in st THEN [turn[i] EXCEPT !.st = "cancelled"] ELSE turn[i]]

Edit ==
  /\ up /\ def.ver < MaxEdits
  /\ \E p \in Persons, b \in BOOLEAN :
       LET d2 == [paused |-> b, person |-> p, ver |-> def.ver + 1]
       IN /\ def' = d2
          /\ defHist' = IF DefInTxn THEN defHist ELSE defHist \cup {d2}
          /\ turn' = IF PauseCancels /\ ~PauseLater /\ b THEN CancelAll(Pending \cup {"claimed"}) ELSE turn
  /\ UNCHANGED <<now, wm, up, revoked, repoch, cut, held, fires, ans, pend, revHist>>

\* PauseLater: the cancel of a pause is its own transaction (broken variant).
CancelPaused ==
  /\ up /\ PauseCancels /\ PauseLater /\ def.paused
  /\ \E i \in Ids : turn[i].st \in Pending \cup {"claimed"}
  /\ turn' = CancelAll(Pending \cup {"claimed"})
  /\ UNCHANGED <<now, wm, up, def, revoked, repoch, cut, held, fires, ans, pend, defHist, revHist>>

\* The authority revoke (M2): visible to every may check from here on. revoke_epoch does not move yet.
RevokeAuth(p) ==
  /\ up /\ ~revoked[p] /\ repoch[p] < MaxRevs
  /\ LET r2 == [revoked EXCEPT ![p] = TRUE]
     IN /\ revoked' = r2 /\ cut' = [cut EXCEPT ![p] = FALSE]
        /\ revHist' = IF MayInTxn THEN revHist ELSE revHist \cup {[rv |-> r2, ep |-> repoch]}
  /\ UNCHANGED <<now, wm, up, def, repoch, held, fires, turn, ans, pend, defHist>>

\* claims.rs revoke_person, in its own transaction after the authority revoke: it cancels the person's unfinished turns
\* and bumps revoke_epoch, so a fire or claim that checked may before it commits is refused (revoke_seen).
RevokePerson(p) ==
  /\ up /\ revoked[p] /\ ~cut[p]
  /\ cut' = [cut EXCEPT ![p] = TRUE]
  /\ LET e2 == [repoch EXCEPT ![p] = repoch[p] + 1]
     IN /\ repoch' = e2
        /\ revHist' = IF MayInTxn THEN revHist ELSE revHist \cup {[rv |-> revoked, ep |-> e2]}
  /\ turn' = IF RevokeCancels
               THEN [i \in Ids |-> IF turn[i].person = p /\ turn[i].st \in Unfin
                                   THEN [turn[i] EXCEPT !.st = "cancelled"] ELSE turn[i]]
               ELSE turn
  /\ UNCHANGED <<now, wm, up, def, revoked, held, fires, ans, pend, defHist>>

Grant(p) ==
  /\ up /\ revoked[p] /\ cut[p]
  /\ LET r2 == [revoked EXCEPT ![p] = FALSE]
     IN /\ revoked' = r2 /\ cut' = [cut EXCEPT ![p] = FALSE]
        /\ revHist' = IF MayInTxn THEN revHist ELSE revHist \cup {[rv |-> r2, ep |-> repoch]}
  /\ UNCHANGED <<now, wm, up, def, repoch, held, fires, turn, ans, pend, defHist>>

Crash ==
  /\ up /\ up' = FALSE /\ pend' = NoPend
  /\ turn' = [i \in Ids |-> IF turn[i].st = "claimed" THEN [turn[i] EXCEPT !.st = "queued"] ELSE turn[i]]
  /\ UNCHANGED <<now, wm, def, revoked, repoch, cut, held, fires, ans, defHist, revHist>>

Restart ==
  /\ ~up /\ up' = TRUE
  /\ UNCHANGED <<now, wm, def, revoked, repoch, cut, held, fires, turn, ans, pend, defHist, revHist>>

Hold == /\ Holds /\ ~held /\ held' = TRUE /\ UNCHANGED <<now, wm, up, def, revoked, repoch, cut, fires, turn, ans, pend, defHist, revHist>>
Release == /\ held /\ held' = FALSE /\ UNCHANGED <<now, wm, up, def, revoked, repoch, cut, fires, turn, ans, pend, defHist, revHist>>

\* Time runs while the core is down; with the guard it cannot pass an instant at which an up core ran no tick.
Advance ==
  /\ now < MaxNow
  /\ TickGuard => (up => wm >= now)
  /\ now' = now + 1
  /\ UNCHANGED <<wm, up, def, revoked, repoch, cut, held, fires, turn, ans, pend, defHist, revHist>>

\* ---- the turn lifecycle (M3-2: queued -> claimed -> running -> done, parked, expired) ------------------------------

TurnStep(i, new) ==
  /\ turn' = [turn EXCEPT ![i].st = new]
  /\ UNCHANGED <<now, wm, up, def, revoked, repoch, cut, held, fires, ans, pend, defHist, revHist>>

\* A queued turn tries the conversation: held -> parked (lock_busy); free -> claimed. A parked turn is re-probed.
\* The claim's may check is the caller's, taken before the transaction (claims.rs:32-37): it may have read any past
\* authority snapshot, and the claim is refused only when revoke_epoch moved since (revoke_seen). So a claim whose may
\* check ran before the authority revoke still commits until revoke_person bumps the epoch; what stops a late claim is
\* the turn's state (revoke_person cancels the person's pending turns in the epoch's transaction).
ClaimMayOk(i) ==
  ~ClaimMay \/ \E a \in AuthChoices : ~a.rv[turn[i].person] /\ a.ep[turn[i].person] = repoch[turn[i].person]

Claim(i) ==
  /\ up /\ turn[i].st \in Pending
  /\ (~DeadlineCheck \/ now < turn[i].dl)
  /\ IF held THEN turn[i].st = "queued" /\ TurnStep(i, "parked")
     ELSE ClaimMayOk(i) /\ TurnStep(i, "claimed")

Expire(i) == /\ up /\ turn[i].st \in Pending /\ turn[i].dl <= now /\ TurnStep(i, "expired")
Start(i) == /\ up /\ turn[i].st = "claimed" /\ TurnStep(i, "running")
Finish(i) == /\ up /\ turn[i].st = "running" /\ TurnStep(i, "done")

ClaimAny == \E i \in Ids : Claim(i)
ExpireAny == \E i \in Ids : Expire(i)
StartAny == \E i \in Ids : Start(i)
FinishAny == \E i \in Ids : Finish(i)

Next ==
  \/ TickFresh \/ TickStale \/ AdHocFire \/ FireB
  \/ Edit \/ CancelPaused \/ (\E p \in Persons : RevokeAuth(p) \/ RevokePerson(p) \/ Grant(p))
  \/ Crash \/ Restart \/ Hold \/ Release \/ Advance
  \/ ClaimAny \/ ExpireAny \/ StartAny \/ FinishAny

\* The core may crash any number of times. A crash loop (claim -> crash -> restart -> claim ...) does not starve a turn:
\* strong fairness lets it run in the stretches where the core is up (the review replaced "crashes finitely often").
Fairness ==
  /\ WF_vars(Advance) /\ WF_vars(Restart) /\ WF_vars(FireB)
  /\ SF_vars(TickFresh) /\ SF_vars(ClaimAny) /\ SF_vars(ExpireAny) /\ SF_vars(StartAny) /\ SF_vars(FinishAny)
  /\ (Redeliver => SF_vars(AdHocFire))

Spec == Init /\ [][Next]_vars /\ Fairness

\* ---- properties --------------------------------------------------------------------------------------------------

Outcomes == {"fired", "skip_prev", "skip_paused", "skip_revoked"}

TypeOK ==
  /\ now \in 0..MaxNow /\ wm \in 0..MaxNow /\ up \in BOOLEAN /\ held \in BOOLEAN
  /\ def.ver \in 0..MaxEdits /\ revoked \in [Persons -> BOOLEAN] /\ cut \in [Persons -> BOOLEAN]
  /\ repoch \in [Persons -> 0..MaxRevs]
  /\ \A s \in Slots : fires[s].out \in {"none", "skip_missed"} \cup Outcomes
  /\ \A s \in Slots : ans[s] \in {"none", "busy"} \cup Outcomes
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

\* After revoke_person has run for a revoke, nothing of the person is queued, parked, claimed or running.
NoRunForRevoked == \A i \in Ids : turn[i].st # "none" => ((revoked[turn[i].person] /\ cut[turn[i].person]) => turn[i].st \notin Unfin)

\* A catch-up never runs a slot older than the window.
CatchUpWindow == \A s \in Timed : (fires[s].out = "fired" /\ fires[s].src = "tick") => fires[s].n - P * s < Catch

\* One fire per missed span: no two fires of one tick cover (the same clock reading, different slots).
CatchUpOnce ==
  \A s1, s2 \in Timed : (s1 # s2 /\ fires[s1].out = "fired" /\ fires[s2].out = "fired" /\ fires[s1].src = "tick"
                         /\ fires[s2].src = "tick") => fires[s1].n # fires[s2].n

\* Requirement (j): every slot at or before the watermark has a row (fired, skipped with its reason, or skipped:missed).
\* (Outside a split fire's pending step.)
WatermarkRows == pend.on \/ \A k \in Timed : P * k <= wm => fires[k].out # "none"

\* The answer to a run-now / webhook caller: a decided answer is the recorded outcome.
AnswerIsRecorded == \A s \in AdHoc : ans[s] \notin {"none", "busy"} => ans[s] = fires[s].out

\* Action properties: what a step may create or start.
\* (A fire that commits between the authority revoke and revoke_person is cancelled by revoke_person; after it, refused.)
NoFireForRevokedPerson ==
  [][\A i \in Ids : (turn[i].st = "none" /\ turn'[i].st # "none")
        => ~(revoked'[turn'[i].person] /\ cut'[turn'[i].person])]_vars
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
\* After revoke_person has run for a revoke, no turn of the person is claimed. (Between the authority revoke and
\* revoke_person a claim with a stale may check can still commit; revoke_person then cancels it, NoRunForRevoked.)
RevokedNeverStarts ==
  [][\A i \in Ids : (turn[i].st # "claimed" /\ turn'[i].st = "claimed")
        => ~(revoked'[turn'[i].person] /\ cut'[turn'[i].person])]_vars
\* A skip is recorded only when a live turn exists: not a parked turn past its deadline that no sweep has reached.
SkipIsJustified ==
  [][\A s \in Slots : (fires[s].out # "skip_prev" /\ fires'[s].out = "skip_prev")
       => \E i \in Ids : turn[i].st \in Unfin /\ ~(turn[i].st \in Pending /\ turn[i].dl <= fires'[s].n)]_vars
\* A new turn's deadline is after the clock its fire read: it is not dead on arrival (requirement 6).
FiredIsLive ==
  [][\A i \in Ids : (turn[i].st = "none" /\ turn'[i].st # "none") => turn'[i].dl > fires'[turn'[i].slot].n]_vars
\* A busy answer writes no key (requirement 8): the retry finds the id undecided.
BusyWritesNoKey ==
  [][\A s \in AdHoc : (ans'[s] = "busy" /\ ans[s] # "busy") => fires' = fires]_vars

\* Liveness: the last slot gets a row (fired or skipped with its reason) once its time is over; a turn whose deadline is
\* inside the model ends; every webhook delivery that is retried until accepted gets its turn (no revoke, no pause, no
\* hold: only the previous run can make it busy).
DueGetsRow == (now = MaxNow) ~> (fires[NS].out # "none")
TurnsEnd == \A i \in Ids : ((turn[i].st \in Unfin /\ turn[i].dl <= MaxNow) ~> (turn[i].st \notin Unfin))
HookRuns == \A s \in AdHoc : <>(fires[s].out = "fired")

\* Non-vacuity: each of these happens (the invariant FAILS).
NeverFired == \A s \in Slots : fires[s].out # "fired"
NeverSkipPrev == \A s \in Slots : fires[s].out # "skip_prev"
NeverSkipPaused == \A s \in Slots : fires[s].out # "skip_paused"
NeverSkipRevoked == \A s \in Slots : fires[s].out # "skip_revoked"
NeverSkipMissed == \A s \in Slots : fires[s].out # "skip_missed"
NeverAdHocFired == \A s \in AdHoc : fires[s].out # "fired"
NeverBusy == \A s \in AdHoc : ans[s] # "busy"
NeverCaughtUp == \A s \in Timed : ~(fires[s].out = "fired" /\ fires[s].n > P * s)
NeverParked == \A i \in Ids : turn[i].st # "parked"
NeverExpired == \A i \in Ids : turn[i].st # "expired"
NeverDone == \A i \in Ids : turn[i].st # "done"
NeverCancelled == \A i \in Ids : turn[i].st # "cancelled"
NeverDown == up
====
