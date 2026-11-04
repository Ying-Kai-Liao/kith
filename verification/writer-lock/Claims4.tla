------------------------------ MODULE Claims4 ------------------------------
(***************************************************************************)
(* M3-10b1: the rows of core.db against the processes of one conversation  *)
(* (round 4 of the writer-lock model, the part WriterLock4.tla leaves out). *)
(* WriterLock4 is the lock and the holder; Holder.tla is the holder's       *)
(* spawn rules. This module is the claim and turn rows of                   *)
(* core/runtime/src/conversations/claims.rs and the ask stop of             *)
(* core/runtime/src/asks/mod.rs (end_tree), one function per action:        *)
(*                                                                          *)
(*   MayCheck/TurnArrive  the caller's may check reads revoke_epoch, then   *)
(*                        enqueue_turn                                      *)
(*   Claim                claim_in (reconciled, epoch, the stopped-ask      *)
(*                        fence, one writer, the turn CAS)                  *)
(*   Spawn/SpawnedRep     the holder starts the stub; `spawned` records pid *)
(*   Redeem               redeem (CAS claimed -> running) and the stub's    *)
(*                        lock; the stub of a released claim exits          *)
(*   Busy/Report          report: busy parks the turn, done ends the claim  *)
(*   AskStop              asks::end_tree -> stop_conversation_in -> stop_in *)
(*   Revoke               revoke_person                                     *)
(*   TakeOver/PromoteTW   a queued take-over claim; promotion is NOT in the *)
(*                        code yet (claims.rs header: M3-11) - modelled as  *)
(*                        the rule it must follow                           *)
(*   SendKill/HKill       the kill the stop returns, re-sent until gone     *)
(*   EndGone              end_gone: ending -> ended once the pid is gone    *)
(*   HCrash/HUp/Meet      holder crash (its group survives or dies with     *)
(*                        it), new instance, different_holder               *)
(*   TurnArrive of an Auto turn after the stop = a wake given back          *)
(*                        (wake/queue.rs give_back/release) or a card       *)
(*                        resume: a new ask or wake turn for the stopped    *)
(*                        ask's conversation.                               *)
(*                                                                          *)
(* One conversation, one person P whose turns they are (the Auto turns are  *)
(* P's turns of source ask or wake), one owner O whose take-over claims are *)
(* never revoked. The lock is "a process of another claim is writing".      *)
(*                                                                          *)
(* Source: origin/rust-rebuild cf308ebc (the one sha for this module and    *)
(* the skill's table).                                                      *)
(*                                                                          *)
(* Two places where the model is not the code, on purpose:                  *)
(*  - TakeOver also cancels the live turn (ts claimed/running -> cancelled) *)
(*    and stops its claim. That is the M3-11 rule (the terminal entrance);  *)
(*    no code does it yet (claims.rs header: promotion is M3-11). The model *)
(*    checks the rule it must follow, like PromoteTW.                       *)
(*  - Claim's gate is the code's: claim_in refuses until the core           *)
(*    generation's reconcile committed (claims.rs:268), and nothing about   *)
(*    the holder being up. A claim made while the holder is down waits for  *)
(*    it (Spawn needs hUp) and the new instance's Meet releases it.         *)
(*                                                                          *)
(* A refused claim (epoch: claims.rs:273; stopped-ask fence: :292-309, now  *)
(* Error::Stopped) leaves its turn `queued`: claim_in's Err rolls back its  *)
(* own transaction, so the cancel can only be done by the CALLER after the  *)
(* refusal, in a transaction of its own (a drain, M3-10). The fencecancel   *)
(* variant is that caller-side cancel, for both refusals.                   *)
(*                                                                          *)
(* Variant (default "fix" = the code at origin/rust-rebuild cf308ebc):      *)
(*   nofence       claim does not refuse an ask/wake turn of a stopped ask  *)
(*   fencecancel   the caller cancels a turn claim_in refused (stopped ask  *)
(*                 or stale revoke epoch); the code leaves it queued: see   *)
(*                 P3Strict (c4-fenceleak, c4-epochleak)                    *)
(*   noepoch       claim does not compare the revoke epoch                  *)
(*   endearly      end_gone ends `ending` without the process being gone    *)
(*   doneoverwrites a late done report marks a cancelled turn done          *)
(*   nokillresend  the kill is sent once and different_holder does not      *)
(*                 owe it again                                             *)
(*   promoteearly  a queued take-over claim is promoted with a live claim   *)
(*   stopall       the ask stop also takes the person's take-over claim     *)
(* HMode: on a holder crash its group "survive"s or "die"s with it.         *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS Variant, HMode, Turns, Auto, MaxClaims, MaxHC, MaxRev, MaxTO

Claims == 1..MaxClaims

VARIABLES
  cs, ct, ck, cp,          \* claim rows: state, turn (0 = none), kind, pid recorded
  pr, ex, sp, kp, km,      \* processes: state; the holder's kept report; spawn sent; kill owed; kill in flight
  nextC, nTO,              \* claim counter, take-overs asked
  ts, ms,                  \* turn rows: state; the revoke epoch its may check read (-1 = not checked)
  rev, aSt,                \* revoke epoch of P; the ask's state
  hUp, hc, synced          \* holder up, crashes, the core has met this instance

claimV == <<cs, ct, ck, cp, nextC>>
procV  == <<pr, ex, sp, kp, km>>
turnV  == <<ts, ms>>
hV     == <<hUp, hc, synced>>
vars   == <<claimV, procV, turnV, rev, aSt, nTO, hV>>

Live(c)   == cs[c] \in {"claimed", "running", "ending"}
AnyLive   == \E c \in Claims : Live(c)
Alive(c)  == pr[c] \in {"stub", "writing"}
Writing   == \E c \in Claims : pr[c] = "writing"
Fenced(t) == t \in Auto /\ aSt = "stopped" /\ Variant # "nofence"

Init ==
  /\ cs = [c \in Claims |-> "none"] /\ ct = [c \in Claims |-> 0] /\ ck = [c \in Claims |-> "turn"]
  /\ cp = [c \in Claims |-> FALSE] /\ nextC = 0
  /\ pr = [c \in Claims |-> "none"] /\ ex = [c \in Claims |-> "none"] /\ sp = [c \in Claims |-> FALSE]
  /\ kp = [c \in Claims |-> FALSE] /\ km = [c \in Claims |-> FALSE]
  /\ ts = [t \in Turns |-> "new"] /\ ms = [t \in Turns |-> -1]
  /\ rev = 0 /\ aSt = "open" /\ nTO = 0
  /\ hUp = TRUE /\ hc = 0 /\ synced = TRUE

---------------------------------------------------------------------------
(* Turns *)
MayCheck(t) ==   \* revoke_epoch before the may check; a revoked person passes nothing
  /\ ts[t] = "new" /\ ms[t] = -1 /\ rev = 0
  /\ ms' = [ms EXCEPT ![t] = rev]
  /\ UNCHANGED <<claimV, procV, ts, rev, aSt, nTO, hV>>

TurnArrive(t) ==   \* enqueue_turn; after the stop this is a wake given back or a card resume
  /\ ts[t] = "new" /\ ms[t] # -1
  /\ ts' = [ts EXCEPT ![t] = "queued"]
  /\ UNCHANGED <<claimV, procV, ms, rev, aSt, nTO, hV>>

Park(t) ==   \* park(lock_busy | terminal_live)
  /\ ts[t] = "queued" /\ ts' = [ts EXCEPT ![t] = "parked"]
  /\ UNCHANGED <<claimV, procV, ms, rev, aSt, nTO, hV>>

Unpark(t) ==   \* unpark_ready: no live claim and the lock free (probed outside the transaction)
  /\ ts[t] = "parked" /\ ~AnyLive /\ ~Writing
  /\ ts' = [ts EXCEPT ![t] = "queued"]
  /\ UNCHANGED <<claimV, procV, ms, rev, aSt, nTO, hV>>

FenceCancel(t) ==   \* variant fencecancel: the caller cancels a turn claim_in refused (fence or stale epoch)
  /\ Variant = "fencecancel" /\ (Fenced(t) \/ ms[t] # rev) /\ ts[t] \in {"queued", "parked"}
  /\ ts' = [ts EXCEPT ![t] = "cancelled"]
  /\ UNCHANGED <<claimV, procV, ms, rev, aSt, nTO, hV>>

---------------------------------------------------------------------------
(* Claim, spawn, redeem, report *)
Claim(t) ==   \* claim_in: gated on the core's reconcile only (not modelled: always done), not on the holder
  /\ nextC < MaxClaims /\ ~AnyLive
  /\ ts[t] \in {"queued", "parked"}
  /\ (Variant = "noepoch" \/ ms[t] = rev)
  /\ ~Fenced(t)
  /\ LET c == nextC + 1 IN
       /\ cs' = [cs EXCEPT ![c] = "claimed"] /\ ct' = [ct EXCEPT ![c] = t] /\ ck' = [ck EXCEPT ![c] = "turn"]
       /\ cp' = cp /\ nextC' = c
       /\ sp' = [sp EXCEPT ![c] = TRUE]
  /\ ts' = [ts EXCEPT ![t] = "claimed"]
  /\ UNCHANGED <<pr, ex, kp, km, ms, rev, aSt, nTO, hV>>

Spawn(c) ==   \* the holder starts the stub
  /\ hUp /\ sp[c] /\ sp' = [sp EXCEPT ![c] = FALSE] /\ pr' = [pr EXCEPT ![c] = "stub"]
  /\ UNCHANGED <<claimV, ex, kp, km, turnV, rev, aSt, nTO, hV>>

SpawnedRep(c) ==   \* report `spawned`: record the stub's pid
  /\ hUp /\ pr[c] = "stub" /\ cs[c] = "claimed" /\ ~cp[c]
  /\ cp' = [cp EXCEPT ![c] = TRUE]
  /\ UNCHANGED <<cs, ct, ck, nextC, procV, turnV, rev, aSt, nTO, hV>>

Redeem(c) ==   \* the stub: CAS claimed -> running while it holds the lock; a released claim's stub exits
  /\ pr[c] = "stub"
  /\ IF cs[c] = "claimed"
       THEN /\ \A d \in Claims \ {c} : pr[d] # "writing"
            /\ cs' = [cs EXCEPT ![c] = "running"] /\ cp' = [cp EXCEPT ![c] = TRUE]
            /\ pr' = [pr EXCEPT ![c] = "writing"]
            /\ ts' = IF ct[c] # 0 THEN [ts EXCEPT ![ct[c]] = "running"] ELSE ts
       ELSE /\ pr' = [pr EXCEPT ![c] = "dead"]      \* redeem refused
            /\ UNCHANGED <<cs, cp, ts>>
  /\ UNCHANGED <<ct, ck, nextC, ex, sp, kp, km, ms, rev, aSt, nTO, hV>>

Busy(c) ==   \* the stub finds the lock taken: exits, the holder keeps `busy`
  /\ pr[c] = "stub" /\ cs[c] = "claimed" /\ \E d \in Claims \ {c} : pr[d] = "writing"
  /\ pr' = [pr EXCEPT ![c] = "dead"] /\ ex' = [ex EXCEPT ![c] = IF hUp THEN "busy" ELSE "none"]
  /\ UNCHANGED <<claimV, sp, kp, km, turnV, rev, aSt, nTO, hV>>

EngineExit(c) ==   \* the engine ends its turn; the holder keeps the report until acked
  /\ pr[c] = "writing" /\ pr' = [pr EXCEPT ![c] = "dead"]
  /\ ex' = [ex EXCEPT ![c] = IF hUp THEN "done" ELSE "none"]
  /\ UNCHANGED <<claimV, sp, kp, km, turnV, rev, aSt, nTO, hV>>

Report(c) ==   \* claims::report: done ends a running or ending claim, busy a claimed one
  /\ hUp /\ ex[c] # "none"
  /\ ex' = [ex EXCEPT ![c] = "none"]
  /\ IF ex[c] = "done"
       THEN /\ cs' = [cs EXCEPT ![c] = IF cs[c] \in {"running", "ending"} THEN "ended" ELSE cs[c]]
            /\ ts' = IF ct[c] # 0 /\ cs[c] = "running" /\ ts[ct[c]] = "running"
                       THEN [ts EXCEPT ![ct[c]] = "done"]
                     ELSE IF ct[c] # 0 /\ cs[c] = "ending" /\ ts[ct[c]] = "cancelled" /\ Variant = "doneoverwrites"
                       THEN [ts EXCEPT ![ct[c]] = "done"]
                     ELSE ts
       ELSE /\ cs' = [cs EXCEPT ![c] = IF cs[c] = "claimed" THEN "ended" ELSE cs[c]]
            /\ ts' = IF ct[c] # 0 /\ cs[c] = "claimed" /\ ts[ct[c]] = "claimed"
                       THEN [ts EXCEPT ![ct[c]] = "parked"] ELSE ts
  /\ UNCHANGED <<ct, ck, cp, nextC, pr, sp, kp, km, ms, rev, aSt, nTO, hV>>

---------------------------------------------------------------------------
(* Stop, revoke, take-over: claim goes `ending` (or `ended` when never spawned), the turn is cancelled, a kill is owed *)
AfterStop(c) == IF cs[c] = "claimed" /\ ~cp[c] THEN "ended"
                ELSE IF cs[c] \in {"claimed", "running"} THEN "ending" ELSE cs[c]

StopsTurnClaim(c) == ck[c] = "turn" \/ Variant = "stopall"

AskStop ==   \* end_tree: queued and parked turns cancelled, live turn claims stopped, the ask stopped
  /\ aSt = "open"
  /\ aSt' = "stopped"
  /\ cs' = [c \in Claims |-> IF StopsTurnClaim(c) THEN AfterStop(c) ELSE cs[c]]
  /\ kp' = [c \in Claims |-> IF StopsTurnClaim(c) /\ AfterStop(c) = "ending" THEN TRUE ELSE kp[c]]
  /\ ts' = [t \in Turns |-> IF ts[t] \in {"queued", "parked", "claimed", "running"} THEN "cancelled" ELSE ts[t]]
  /\ UNCHANGED <<ct, ck, cp, nextC, pr, ex, sp, km, ms, rev, nTO, hV>>

Revoke ==   \* revoke_person: the epoch moves, P's turns are cancelled, P's live claims stopped
  /\ rev < MaxRev /\ rev' = rev + 1
  /\ cs' = [c \in Claims |-> IF ck[c] = "turn" THEN AfterStop(c) ELSE cs[c]]
  /\ kp' = [c \in Claims |-> IF ck[c] = "turn" /\ AfterStop(c) = "ending" THEN TRUE ELSE kp[c]]
  /\ ts' = [t \in Turns |-> IF ts[t] \in {"queued", "parked", "claimed", "running"} THEN "cancelled" ELSE ts[t]]
  /\ UNCHANGED <<ct, ck, cp, nextC, pr, ex, sp, km, ms, aSt, nTO, hV>>

TakeOver ==   \* the owner's session: a queued take-over claim, the live turn claim stopped to free the writer
  /\ nTO < MaxTO /\ nextC < MaxClaims /\ nTO' = nTO + 1
  /\ ~\E d \in Claims : cs[d] = "queued"   \* claims_one_queued (m0080): at most one queued claim per conversation
  /\ LET c == nextC + 1 IN
       /\ cs' = [d \in Claims |-> IF d = c THEN "queued" ELSE IF ck[d] = "turn" THEN AfterStop(d) ELSE cs[d]]
       /\ ck' = [ck EXCEPT ![c] = "tw"] /\ nextC' = c /\ ct' = ct /\ cp' = cp
       /\ kp' = [d \in Claims |-> IF d # c /\ ck[d] = "turn" /\ AfterStop(d) = "ending" THEN TRUE ELSE kp[d]]
  /\ ts' = [t \in Turns |-> IF ts[t] \in {"claimed", "running"} THEN "cancelled" ELSE ts[t]]
  /\ UNCHANGED <<pr, ex, sp, km, ms, rev, aSt, hV>>

PromoteTW(c) ==   \* queued -> claimed once nothing is the writer (the code lacks this: M3-11)
  /\ hUp /\ synced /\ ck[c] = "tw" /\ cs[c] = "queued" /\ (Variant = "promoteearly" \/ ~AnyLive)
  /\ cs' = [cs EXCEPT ![c] = "claimed"] /\ sp' = [sp EXCEPT ![c] = TRUE]
  /\ UNCHANGED <<ct, ck, cp, nextC, pr, ex, kp, km, turnV, rev, aSt, nTO, hV>>

SendKill(c) ==   \* the kill the stop returned, sent to the holder; resent while the claim is `ending`
  /\ hUp /\ cs[c] = "ending" /\ kp[c]
  /\ km' = [km EXCEPT ![c] = TRUE]
  /\ kp' = [kp EXCEPT ![c] = Variant # "nokillresend"]
  /\ UNCHANGED <<claimV, pr, ex, sp, turnV, rev, aSt, nTO, hV>>

HKill(c) ==   \* the holder kills the group and keeps the exit
  /\ hUp /\ km[c]
  /\ km' = [km EXCEPT ![c] = FALSE]
  /\ pr' = [pr EXCEPT ![c] = IF Alive(c) THEN "dead" ELSE pr[c]]
  /\ ex' = [ex EXCEPT ![c] = IF Alive(c) THEN "done" ELSE ex[c]]
  /\ UNCHANGED <<claimV, sp, kp, turnV, rev, aSt, nTO, hV>>

EndGone(c) ==   \* ending -> ended once the recorded process is confirmed gone
  /\ cs[c] = "ending" /\ cp[c] /\ (pr[c] = "dead" \/ Variant = "endearly")
  /\ cs' = [cs EXCEPT ![c] = "ended"]
  /\ UNCHANGED <<ct, ck, cp, nextC, procV, turnV, rev, aSt, nTO, hV>>

---------------------------------------------------------------------------
(* Holder *)
HCrash ==
  /\ hUp /\ hc < MaxHC /\ hUp' = FALSE /\ hc' = hc + 1 /\ synced' = FALSE
  /\ sp' = [c \in Claims |-> FALSE] /\ km' = [c \in Claims |-> FALSE] /\ ex' = [c \in Claims |-> "none"]
  /\ pr' = IF HMode = "die" THEN [c \in Claims |-> IF Alive(c) THEN "dead" ELSE pr[c]] ELSE pr
  /\ UNCHANGED <<claimV, kp, turnV, rev, aSt, nTO>>

HUp ==
  /\ ~hUp /\ hUp' = TRUE
  /\ UNCHANGED <<claimV, procV, turnV, rev, aSt, nTO, hc, synced>>

Meet ==   \* different_holder: the new instance lists nothing, so every live non-external claim is released
  /\ hUp /\ ~synced /\ synced' = TRUE
  /\ cs' = [c \in Claims |->
        IF ~Live(c) THEN cs[c]
        ELSE IF cs[c] = "claimed" THEN "ended"
        ELSE IF cs[c] = "running" THEN "ending"
        ELSE IF cp[c] THEN "ending" ELSE "ended"]
  /\ ts' = [t \in Turns |-> IF ts[t] \in {"claimed", "running"} THEN "queued" ELSE ts[t]]
  /\ kp' = [c \in Claims |-> IF Variant # "nokillresend" /\ cs[c] \in {"running", "ending"} /\ cp[c] THEN TRUE ELSE kp[c]]
  /\ UNCHANGED <<ct, ck, cp, nextC, pr, ex, sp, km, ms, rev, aSt, nTO, hUp, hc>>

---------------------------------------------------------------------------
Next ==
  \/ \E t \in Turns : MayCheck(t) \/ TurnArrive(t) \/ Park(t) \/ Unpark(t) \/ Claim(t) \/ FenceCancel(t)
  \/ \E c \in Claims : Spawn(c) \/ SpawnedRep(c) \/ Redeem(c) \/ Busy(c) \/ EngineExit(c) \/ Report(c)
                       \/ PromoteTW(c) \/ SendKill(c) \/ HKill(c) \/ EndGone(c)
  \/ AskStop \/ Revoke \/ TakeOver \/ HCrash \/ HUp \/ Meet

Fairness ==
  /\ \A t \in Turns : WF_vars(Unpark(t)) /\ WF_vars(Claim(t)) /\ WF_vars(FenceCancel(t))
  /\ \A c \in Claims : WF_vars(Spawn(c)) /\ WF_vars(Redeem(c)) /\ WF_vars(Busy(c)) /\ WF_vars(EngineExit(c) /\ cs[c] # "ending")   \* an engine under a stop may hang: only the kill ends it
                       /\ WF_vars(Report(c)) /\ WF_vars(PromoteTW(c)) /\ WF_vars(SendKill(c))
                       /\ WF_vars(HKill(c)) /\ WF_vars(EndGone(c))
  /\ WF_vars(HUp) /\ WF_vars(Meet)

Spec == Init /\ [][Next]_vars /\ Fairness

---------------------------------------------------------------------------
TypeOK ==
  /\ \A c \in Claims : cs[c] \in {"none", "queued", "claimed", "running", "ending", "ended"}
  /\ \A t \in Turns : ts[t] \in {"new", "queued", "parked", "claimed", "running", "done", "cancelled"}

\* claims_one_writer: the partial unique index.
OneLive == Cardinality({c \in Claims : Live(c)}) <= 1
\* One process writes at a time.
OneWriter == Cardinality({c \in Claims : pr[c] = "writing"}) <= 1
\* A writing process's claim is the writer (running, or ending until it is gone): never ended while it writes.
WriterHasClaim == \A c \in Claims : pr[c] = "writing" => cs[c] \in {"running", "ending"}
\* After the ask stopped, no ask or wake turn of it is claimed or running (a stopped child runs again otherwise).
StoppedNeverRuns == aSt = "stopped" => \A t \in Auto : ts[t] \notin {"claimed", "running"}
\* After a revoke, none of the person's turns is claimed or running.
RevokedQuiet == rev > 0 => \A t \in Turns : ts[t] \notin {"claimed", "running"}
\* A turn that was cancelled stays cancelled (a late done report does not undo a stop).
CancelledStays == [][\A t \in Turns : ts[t] = "cancelled" => ts'[t] = "cancelled"]_vars
\* The ask stop spares the owner's take-over claim ("a person's writers are left alone").
AskSparesTakeOver == [][(aSt = "open" /\ aSt' = "stopped") => \A c \in Claims : ck[c] = "tw" => cs'[c] = cs[c]]_vars

\* Every stop ends: an `ending` claim ends once the kill gets through (resent after a holder crash).
EndingEnds == \A c \in Claims : (cs[c] = "ending") ~> (cs[c] = "ended")
\* A turn the claims layer accepts ends done or cancelled. Exempt: a turn whose may check predates a revoke and one of a
\* stopped ask (both are refused for ever; P3Strict says that is a leak). The `nextC = MaxClaims` escape is only the bound
\* of the model (a turn that waits when the claim numbers ran out); it hides no stuck state: P3Strict has it too, and
\* c4-epochcancel / c4-fencecancel pass with it, so a fix is shown by cancelling, not by the escape.
P3 == \A t \in Turns :
        (ts[t] = "queued" /\ ms[t] = rev /\ ~Fenced(t)) ~> (ts[t] \in {"done", "cancelled"} \/ nextC = MaxClaims)
P3Strict == \A t \in Turns :
        (ts[t] = "queued") ~> (ts[t] \in {"done", "cancelled"} \/ nextC = MaxClaims)
=============================================================================
