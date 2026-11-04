---------------------------- MODULE WriterLock4 ----------------------------
(***************************************************************************)
(* Round 4 of the writer lock (runtime-shapes design.md section 4, "The    *)
(* writer lock" + its "Refined after a second architect's review").        *)
(* One conversation, one machine. A warm headless engine group (resident   *)
(* stub = engine's parent, + engine) is spawned by the holder; app turns   *)
(* are prompts to it. Direct runs (`--direct`: the CLI holds the lock as   *)
(* the engine's parent; CLI+engine folded into one process). A take-over   *)
(* writer (tw) is the owner's new session after a take-over.               *)
(* flock is modeled as the set lk of processes holding an fd to the one    *)
(* locked open file description: free iff lk = {}. A close removes only   *)
(* that process's fd (death = close).                                      *)
(* Variant:                                                                *)
(*   "fix"        the refined design                                       *)
(*   "inherit"    the engine holds the lock fd; a child it starts inherits *)
(*   "nostub"     the holder holds warm-turn locks itself                  *)
(*   "goneearly"  holder reports "gone" once the kill is sent              *)
(*   "parkclaim"  busy parks the turn but keeps the claim                  *)
(*   "edge"       wake only on a "released" event (lost on core crash)     *)
(*   "nohash"     no transcript-hash check before prompting a warm process *)
(*   "readlate"   core records the hash when it acks, not the turn end's   *)
(*   "stubfirst"  the stub may die before its engine (killed group)        *)
(* HMode: on holder crash its group "survive"s (detached) or "die"s.       *)
(*                                                                         *)
(* Copied from the architect's folder (2026-11-03-writer-lock/model/       *)
(* WriterLock4.tla, run 2026-11-03 and 2026-11-04; the path is             *)
(* /Users/Kath/kith/agents/architect/files/kith/designs/...). Only the     *)
(* lines marked "M3-0a" are new: open question s.11 Q2 (an idle warm       *)
(* process when a terminal turn arrives) as variant "killidle" + P7.       *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS Variant, HMode, Directs, MaxTurns, MaxCoreCrash, MaxHolderCrash,
          MaxRevoke, MaxTakeOver, MaxChild

Turns == 1..MaxTurns
TW == 99            \* claim id of the take-over writer's session

VARIABLES
  coreUp, phase, gen, ccrash, knownInst,                              \* core
  claim, cst, killReq, ext, cv, goneSeen, toPending, revokes, tos,    \* core.db
  st,                                                                 \* turn status
  hUp, hInst, hcrash, hGen, hJob, hJobSt, hExitK, hExitT, hExitV, msgs, \* holder
  hJobL, hExitL, gL,              \* claim-id abstraction: job / report / running group belong to the current claim
  stub, eng, gOwner, gTurn, gKilled, ev, child, childUsed,            \* engine group, leaked child
  lk,                                                                 \* fds on the locked file
  dSt, dSaw, twSt,                                                    \* direct runs, take-over writer
  tv, staleP, unlinks, relEv                                          \* transcript version, flags

coreV == <<coreUp, phase, gen, ccrash, knownInst>>
dbV   == <<claim, cst, killReq, ext, cv, goneSeen, toPending, revokes, tos>>
hV    == <<hUp, hInst, hcrash, hGen, hJob, hJobSt, hExitK, hExitT, hExitV, msgs, hJobL, hExitL, gL>>
gV    == <<stub, eng, gOwner, gTurn, gKilled, ev, child, childUsed>>
dV    == <<dSt, dSaw, twSt>>
xV    == <<tv, staleP, unlinks>>
vars  == <<coreV, dbV, st, hV, gV, lk, dV, xV, relEv>>

Init ==
  /\ coreUp = FALSE /\ phase = "down" /\ gen = 0 /\ ccrash = 0 /\ knownInst = 0
  /\ claim = 0 /\ cst = "none" /\ killReq = FALSE /\ ext = "none" /\ cv = 0
  /\ goneSeen = FALSE /\ toPending = FALSE /\ revokes = 0 /\ tos = 0
  /\ st = [t \in Turns |-> "new"]
  /\ hUp = TRUE /\ hInst = 1 /\ hcrash = 0 /\ hGen = 0 /\ hJob = 0 /\ hJobSt = "none"
  /\ hExitK = "none" /\ hExitT = 0 /\ hExitV = 0 /\ msgs = {}
  /\ hJobL = FALSE /\ hExitL = FALSE /\ gL = FALSE
  /\ stub = "none" /\ eng = "none" /\ gOwner = 0 /\ gTurn = 0 /\ gKilled = FALSE /\ ev = 0
  /\ child = "none" /\ childUsed = 0
  /\ lk = {}
  /\ dSt = [d \in Directs |-> "idle"] /\ dSaw = [d \in Directs |-> FALSE] /\ twSt = "none"
  /\ tv = 0 /\ staleP = FALSE /\ unlinks = 0 /\ relEv = FALSE

Ready == coreUp /\ phase = "ready"
Talk  == Ready /\ hUp /\ knownInst = hInst          \* core is connected to this holder
Orphan == (stub = "up" \/ eng # "none") /\ gOwner # hInst
GroupEmpty == stub = "none" /\ eng = "none"
EmptyNow(s, e) == s = "none" /\ e = "none"

\* edge variant: a "released" event reaches the core only for turns parked at that moment
RelUpd == relEv' = IF Variant = "edge" /\ coreUp /\ lk # {} /\ lk' = {}
                        /\ \E t \in Turns : st[t] = "parked"
                     THEN TRUE ELSE relEv

---------------------------------------------------------------------------
(* Turns *)
TurnArrive(t) ==
  /\ st[t] = "new" /\ st' = [st EXCEPT ![t] = "queued"]
  /\ UNCHANGED <<coreV, dbV, hV, gV, lk, dV, xV>>

---------------------------------------------------------------------------
(* Core *)
CoreStart ==
  /\ ~coreUp /\ coreUp' = TRUE /\ phase' = "reconcile" /\ gen' = gen + 1
  /\ UNCHANGED <<ccrash, knownInst, dbV, st, hV, gV, lk, dV, xV>>

CoreCrash ==
  /\ coreUp /\ ccrash < MaxCoreCrash
  /\ coreUp' = FALSE /\ phase' = "down" /\ ccrash' = ccrash + 1 /\ relEv' = FALSE
  /\ UNCHANGED <<gen, knownInst, dbV, st, hV, gV, lk, dV, xV>>

CoreMeetHolder ==   \* a holder with a new instance id: hello + reconcile again
  /\ Ready /\ hUp /\ hInst # knownInst /\ phase' = "reconcile"
  /\ UNCHANGED <<coreUp, gen, ccrash, knownInst, dbV, st, hV, gV, lk, dV, xV>>

\* What the holder lists as its own (a lock it doesn't hold is not listed).
\* Listed by claim id: an entry for an older claim of the same turn is not this claim.
HeldBy(t) == \/ (hJob = t /\ hJobL)
             \/ (hExitK # "none" /\ hExitT = t /\ hExitL)
             \/ (eng = "turn" /\ gOwner = hInst /\ gTurn = t /\ gL)

CoreReconcile ==   \* hello (holder accepts only this gen) + apply the holder's list, one step
  /\ coreUp /\ phase = "reconcile" /\ hUp
  /\ phase' = "ready" /\ hGen' = gen /\ knownInst' = hInst
  /\ killReq' = (killReq \/ Orphan)        \* step 7: kill every group of the old instance
  /\ IF claim \in Turns /\ cst \in {"claimed", "running", "parked"} /\ ~HeldBy(claim) /\ ~Orphan
       THEN /\ claim' = 0 /\ cst' = "none"
            /\ st' = [st EXCEPT ![claim] = "queued"]
       ELSE UNCHANGED <<claim, cst, st>>
  /\ UNCHANGED <<coreUp, gen, ccrash, ext, cv, goneSeen, toPending, revokes, tos,
                 hUp, hInst, hcrash, hJob, hJobSt, hExitK, hExitT, hExitV, msgs, hJobL, hExitL, gL, gV, lk, dV, xV>>

\* Claim ids (design s.4 step 1/6: a fresh id per claim, never reused). Abstracted as "current or
\* not": a new claim makes every older id stale, i.e. in-flight messages, the holder's job, its
\* unacked report and the running group's claim. The core acts only on the current id.
Stale(ms) == {<<m[1], m[2], FALSE>> : m \in ms}
Send(t) == /\ msgs' = IF hUp /\ knownInst = hInst THEN Stale(msgs) \cup {<<gen, t, TRUE>>} ELSE Stale(msgs)
           /\ hJobL' = FALSE /\ hExitL' = FALSE /\ gL' = FALSE

CoreClaim(t) ==
  /\ Ready /\ claim = 0 /\ ext = "none" /\ ~toPending /\ st[t] = "queued"
  /\ claim' = t /\ cst' = "claimed" /\ st' = [st EXCEPT ![t] = "claimed"] /\ Send(t)
  /\ UNCHANGED <<coreV, killReq, ext, cv, goneSeen, toPending, revokes, tos,
                 hUp, hInst, hcrash, hGen, hJob, hJobSt, hExitK, hExitT, hExitV, gV, lk, dV, xV>>

CoreReprobe(t) ==   \* level-triggered wake (reconcile / slow timer); edge: needs the event
  /\ Ready /\ ~toPending /\ st[t] = "parked"
  /\ (Variant = "edge" => relEv)
  /\ IF Variant = "parkclaim"
       THEN claim = t /\ cst = "parked"
       ELSE claim = 0 /\ ext = "none"
  /\ claim' = t /\ cst' = "claimed" /\ st' = [st EXCEPT ![t] = "claimed"] /\ Send(t)
  /\ relEv' = FALSE
  /\ UNCHANGED <<coreV, killReq, ext, cv, goneSeen, toPending, revokes, tos,
                 hUp, hInst, hcrash, hGen, hJob, hJobSt, hExitK, hExitT, hExitV, gV, lk, dV, xV>>

CoreAck ==
  /\ Talk /\ hExitK # "none"
  /\ hExitK' = "none"
  /\ IF hExitL /\ claim = hExitT /\ cst \in {"claimed", "running"}
       THEN IF hExitK = "done"
              THEN /\ claim' = 0 /\ cst' = "none" /\ st' = [st EXCEPT ![hExitT] = "done"]
                   /\ cv' = IF Variant = "readlate" THEN tv ELSE hExitV
            ELSE \* busy
              /\ st' = [st EXCEPT ![hExitT] = "parked"] /\ UNCHANGED cv
              /\ IF Variant = "parkclaim" THEN cst' = "parked" /\ UNCHANGED claim
                 ELSE claim' = 0 /\ cst' = "none"
       ELSE UNCHANGED <<claim, cst, st, cv>>
  /\ UNCHANGED <<coreV, killReq, ext, goneSeen, toPending, revokes, tos,
                 hUp, hInst, hcrash, hGen, hJob, hJobSt, hExitT, hExitV, msgs, hJobL, hExitL, gL, gV, lk, dV, xV>>

CoreKill ==   \* re-sent while the row says kill and the group isn't gone
  /\ Talk /\ killReq /\ ~GroupEmpty /\ ~gKilled
  /\ gKilled' = TRUE
  /\ UNCHANGED <<coreV, dbV, st, hV, stub, eng, gOwner, gTurn, ev, child, childUsed, lk, dV, xV>>

GoneReported == IF Variant = "goneearly" THEN gKilled \/ GroupEmpty ELSE GroupEmpty

CoreGone ==   \* "gone": only now is the claim released / the conversation free
  /\ Talk /\ killReq /\ GoneReported
  /\ killReq' = FALSE /\ goneSeen' = TRUE
  /\ IF claim \in Turns
       THEN /\ claim' = 0 /\ cst' = "none"
            /\ st' = IF cst = "ended" \/ st[claim] \notin {"claimed", "running", "parked"} THEN st
                     ELSE [st EXCEPT ![claim] = "queued"]     \* orphan of a dead holder: requeue
       ELSE UNCHANGED <<claim, cst, st>>
  /\ UNCHANGED <<coreV, ext, cv, toPending, revokes, tos, hV, gV, lk, dV, xV>>

CoreReapExt ==
  /\ Ready /\ ext # "none" /\ dSt[ext] = "gone" /\ ext' = "none"
  /\ UNCHANGED <<coreV, claim, cst, killReq, cv, goneSeen, toPending, revokes, tos, st, hV, gV, lk, dV, xV>>

Revoke ==   \* ends the claim, drops the person's turns, kills their processes (idle ones too)
  /\ Ready /\ revokes < MaxRevoke /\ revokes' = revokes + 1
  /\ st' = [t \in Turns |-> IF st[t] \in {"queued", "parked", "claimed", "running"} THEN "cancelled" ELSE st[t]]
  /\ IF claim \in Turns THEN cst' = "ended" ELSE UNCHANGED cst
  /\ killReq' = (killReq \/ claim \in Turns \/ ~GroupEmpty)
  /\ UNCHANGED <<coreV, claim, ext, cv, goneSeen, toPending, tos, hV, gV, lk, dV, xV>>

TakeOver ==   \* owner takes over a conversation a running engine holds
  /\ Ready /\ tos < MaxTakeOver /\ claim \in Turns /\ cst = "running"
  /\ tos' = tos + 1 /\ cst' = "ended" /\ st' = [st EXCEPT ![claim] = "cancelled"]
  /\ killReq' = TRUE /\ toPending' = TRUE
  /\ UNCHANGED <<coreV, claim, ext, cv, goneSeen, revokes, hV, gV, lk, dV, xV>>

TWStart ==   \* after "gone": the new writer's session is claimed, then waits for the lock
  /\ Ready /\ toPending /\ claim = 0 /\ ~killReq
  /\ claim' = TW /\ toPending' = FALSE /\ twSt' = "wait"
  /\ UNCHANGED <<coreV, cst, killReq, ext, cv, goneSeen, revokes, tos, st, hV, gV, lk, dSt, dSaw, xV>>

TWLock ==
  /\ twSt = "wait" /\ lk = {} /\ lk' = {"tw"} /\ twSt' = "running"
  /\ UNCHANGED <<coreV, dbV, st, hV, gV, dSt, dSaw, xV>>

TWExit ==
  /\ twSt = "running" /\ twSt' = "gone" /\ lk' = lk \ {"tw"} /\ tv' = tv + 1
  /\ UNCHANGED <<coreV, dbV, st, hV, gV, dSt, dSaw, staleP, unlinks>>

CoreReapTW ==
  /\ Ready /\ claim = TW /\ twSt = "gone" /\ claim' = 0
  /\ UNCHANGED <<coreV, cst, killReq, ext, cv, goneSeen, toPending, revokes, tos, st, hV, gV, lk, dV, xV>>

---------------------------------------------------------------------------
(* Holder *)
HCrash ==
  /\ hUp /\ hcrash < MaxHolderCrash
  /\ hUp' = FALSE /\ hcrash' = hcrash + 1 /\ hJob' = 0 /\ hJobSt' = "none"
  /\ hExitK' = "none" /\ msgs' = {}
  /\ IF HMode = "die"
       THEN /\ stub' = "none" /\ eng' = "none" /\ gKilled' = FALSE
            /\ lk' = lk \ {"holder", "stub", "eng"}
       ELSE /\ lk' = lk \ {"holder"} /\ UNCHANGED <<stub, eng, gKilled>>
  /\ UNCHANGED <<coreV, dbV, st, hInst, hGen, hExitT, hExitV, hJobL, hExitL, gL, gOwner, gTurn, ev, child, childUsed, dV, xV>>

HUp ==
  /\ ~hUp /\ hUp' = TRUE /\ hInst' = hInst + 1 /\ hGen' = 0 /\ msgs' = {}
  /\ UNCHANGED <<coreV, dbV, st, hcrash, hJob, hJobSt, hExitK, hExitT, hExitV, hJobL, hExitL, gL, gV, lk, dV, xV>>

HRecv(m) ==
  /\ hUp /\ m \in msgs /\ msgs' = msgs \ {m}
  /\ IF m[1] = hGen
       THEN hJob = 0 /\ hExitK = "none" /\ hJob' = m[2] /\ hJobSt' = "try" /\ hJobL' = m[3]   \* else it waits
       ELSE UNCHANGED <<hJob, hJobSt, hJobL>>                                     \* older generation: refused
  /\ UNCHANGED <<coreV, dbV, st, hUp, hInst, hcrash, hGen, hExitK, hExitT, hExitV, hExitL, gL, gV, lk, dV, xV>>

HTryLock ==
  /\ hUp /\ hJobSt = "try"
  /\ IF lk = {}
       THEN lk' = {"holder"} /\ hJobSt' = "locked" /\ UNCHANGED <<hJob, hExitK, hExitT, hExitL>>
     ELSE /\ hExitK = "none"   \* architect fix: a report never overwrites an unacked one (design: reports kept per claim until acked)
          /\ hExitK' = "busy" /\ hExitT' = hJob /\ hExitL' = hJobL /\ hJob' = 0 /\ hJobSt' = "none"
          /\ UNCHANGED lk
  /\ UNCHANGED <<coreV, dbV, st, hUp, hInst, hcrash, hGen, hExitV, msgs, hJobL, gL, gV, dV, xV>>

LockAfterHandover == CASE Variant = "nostub"  -> lk
                       [] Variant = "inherit" -> (lk \ {"holder"}) \cup {"eng"}
                       [] OTHER               -> (lk \ {"holder"}) \cup {"stub"}

\* claimed -> running (stub's redeem txn, needs the core) + fd handover + prompt write, one step
HStart ==
  /\ hUp /\ hJobSt = "locked" /\ Ready
  /\ hJob' = 0 /\ hJobSt' = "none"
  /\ IF hJobL /\ claim = hJob /\ cst = "claimed"
       THEN LET idleMine == eng = "idle" /\ stub = "up" /\ gOwner = hInst /\ ~gKilled
                fresh    == GroupEmpty \/ (idleMine /\ Variant # "nohash" /\ tv # cv)
            IN /\ GroupEmpty \/ idleMine                 \* an old group still alive: wait for its kill
               /\ cst' = "running" /\ st' = [st EXCEPT ![hJob] = "running"]
               /\ stub' = "up" /\ eng' = "turn" /\ gOwner' = hInst /\ gTurn' = hJob
               /\ ev' = IF fresh THEN tv ELSE ev           \* stale idle process killed, fresh one loads
               /\ staleP' = (staleP \/ (~fresh /\ ev # tv))
               /\ goneSeen' = IF fresh THEN FALSE ELSE goneSeen
               /\ lk' = LockAfterHandover /\ gL' = TRUE
               /\ UNCHANGED <<gKilled, child, childUsed>>
       ELSE \* the claim was ended meanwhile: drop the lock, run nothing
            /\ lk' = lk \ {"holder"}
            /\ UNCHANGED <<cst, st, gV, gL, staleP, goneSeen>>
  /\ UNCHANGED <<coreV, claim, killReq, ext, cv, toPending, revokes, tos,
                 hUp, hInst, hcrash, hGen, hExitK, hExitT, hExitV, msgs, hJobL, hExitL, dV, tv, unlinks>>

TurnEndLock == CASE Variant = "nostub"  -> lk \ {"holder"}
                 [] Variant = "inherit" -> lk \ {"eng"}
                 [] OTHER               -> lk \ {"stub"}

EngTurnEnd ==   \* turn end: the lock is released (stub on the holder's word), exit kept until acked
  /\ eng = "turn" /\ hUp /\ gOwner = hInst /\ hExitK = "none"
  /\ eng' = "idle" /\ tv' = tv + 1 /\ ev' = tv + 1
  /\ hExitK' = "done" /\ hExitT' = gTurn /\ hExitV' = tv + 1 /\ hExitL' = gL
  /\ lk' = TurnEndLock
  /\ UNCHANGED <<coreV, dbV, st, hUp, hInst, hcrash, hGen, hJob, hJobSt, msgs, hJobL, gL,
                 stub, gOwner, gTurn, gKilled, child, childUsed, dV, staleP, unlinks>>

OrphanTurnEnd ==   \* the old holder is gone: nobody tells the stub to release; nothing reported
  /\ eng = "turn" /\ (~hUp \/ gOwner # hInst)
  /\ eng' = "idle" /\ tv' = tv + 1 /\ ev' = tv + 1
  /\ lk' = IF Variant = "inherit" THEN lk \ {"eng"} ELSE lk
  /\ UNCHANGED <<coreV, dbV, st, hV, stub, gOwner, gTurn, gKilled, child, childUsed, dV, staleP, unlinks>>

EngSpawnChild ==   \* a long-lived child (dev server) that escapes the group and outlives the engine
  /\ eng = "turn" /\ child = "none" /\ childUsed < MaxChild
  /\ child' = "up" /\ childUsed' = childUsed + 1
  /\ lk' = IF "eng" \in lk THEN lk \cup {"child"} ELSE lk   \* CLOEXEC: only an engine-held fd leaks
  /\ UNCHANGED <<coreV, dbV, st, hV, stub, eng, gOwner, gTurn, gKilled, ev, dV, xV>>

ChildExit ==   \* never fair: a dev server need not exit
  /\ child = "up" /\ child' = "none" /\ lk' = lk \ {"child"}
  /\ UNCHANGED <<coreV, dbV, st, hV, stub, eng, gOwner, gTurn, gKilled, ev, childUsed, dV, xV>>

EngDie ==
  /\ gKilled /\ eng # "none" /\ eng' = "none" /\ lk' = lk \ {"eng"}
  /\ gKilled' = ~EmptyNow(stub, "none")
  /\ UNCHANGED <<coreV, dbV, st, hV, stub, gOwner, gTurn, ev, child, childUsed, dV, xV>>

StubDie ==   \* fix: the stub waits for its engine (it is the parent) before it exits
  /\ gKilled /\ stub = "up" /\ (eng = "none" \/ Variant = "stubfirst")
  /\ stub' = "none" /\ lk' = lk \ {"stub"}
  /\ gKilled' = ~EmptyNow("none", eng)
  /\ UNCHANGED <<coreV, dbV, st, hV, eng, gOwner, gTurn, ev, child, childUsed, dV, xV>>

---------------------------------------------------------------------------
(* Direct runs: lock first, always; register with a core that answers *)
DStart(d) ==
  /\ dSt[d] = "idle" /\ dSt' = [dSt EXCEPT ![d] = "checked"] /\ dSaw' = [dSaw EXCEPT ![d] = coreUp]
  /\ UNCHANGED <<coreV, dbV, st, hV, gV, lk, twSt, xV>>

DLock(d) ==
  /\ dSt[d] = "checked"
  /\ IF lk = {} THEN lk' = lk \cup {d} /\ dSt' = [dSt EXCEPT ![d] = "locked"]
     ELSE dSt' = [dSt EXCEPT ![d] = "gone"] /\ UNCHANGED lk
  /\ UNCHANGED <<coreV, dbV, st, hV, gV, dSaw, twSt, xV>>

DReg(d) ==
  /\ dSt[d] = "locked"
  /\ IF dSaw[d] /\ coreUp
       THEN /\ phase = "ready"
            /\ (Variant = "killidle" => GroupEmpty)   \* M3-0a: registers only once the idle warm group is gone
            /\ IF claim = 0 /\ ~toPending
                 THEN ext' = d /\ dSt' = [dSt EXCEPT ![d] = "running"] /\ UNCHANGED lk
               ELSE dSt' = [dSt EXCEPT ![d] = "gone"] /\ lk' = lk \ {d} /\ UNCHANGED ext
     ELSE dSt' = [dSt EXCEPT ![d] = "running"] /\ UNCHANGED <<ext, lk>>   \* fallback
  /\ UNCHANGED <<coreV, claim, cst, killReq, cv, goneSeen, toPending, revokes, tos, st, hV, gV, dSaw, twSt, xV>>

DExit(d) ==   \* it wrote the transcript; exit releases the lock
  /\ dSt[d] = "running" /\ dSt' = [dSt EXCEPT ![d] = "gone"] /\ lk' = lk \ {d} /\ tv' = tv + 1
  /\ UNCHANGED <<coreV, dbV, st, hV, gV, dSaw, twSt, staleP, unlinks>>

\* No action deletes the lock file: unlinks never changes.

\* M3-0a, open question s.11 Q2 under the decided default (2026-11-04): when a terminal turn arrives for a
\* conversation with an idle warm process, the core kills that process first (the same kill as a revoke's:
\* killReq, then "gone"), and the terminal registers only after. Variant "killidle" only.
CoreKillIdle ==
  /\ Variant = "killidle" /\ Ready /\ eng = "idle" /\ ~killReq
  /\ \E d \in Directs : dSt[d] = "locked" /\ dSaw[d]
  /\ killReq' = TRUE
  /\ UNCHANGED <<coreV, claim, cst, ext, cv, goneSeen, toPending, revokes, tos, st, hV, gV, lk, dV, xV>>

---------------------------------------------------------------------------
S(A) == A /\ RelUpd

Next ==
  \/ CoreCrash \/ \E t \in Turns : CoreReprobe(t)
  \/ S(\E t \in Turns : TurnArrive(t))
  \/ S(CoreStart) \/ S(CoreMeetHolder) \/ S(CoreReconcile) \/ S(\E t \in Turns : CoreClaim(t))
  \/ S(CoreKillIdle) \/ S(CoreAck) \/ S(CoreKill) \/ S(CoreGone) \/ S(CoreReapExt) \/ S(Revoke) \/ S(TakeOver)
  \/ S(TWStart) \/ S(TWLock) \/ S(TWExit) \/ S(CoreReapTW)
  \/ S(HCrash) \/ S(HUp) \/ S(\E m \in msgs : HRecv(m)) \/ S(HTryLock) \/ S(HStart)
  \/ S(EngTurnEnd) \/ S(OrphanTurnEnd) \/ S(EngSpawnChild) \/ S(ChildExit) \/ S(EngDie) \/ S(StubDie)
  \/ S(\E d \in Directs : DStart(d) \/ DLock(d) \/ DReg(d) \/ DExit(d))

Fairness ==
  /\ WF_vars(S(CoreStart)) /\ WF_vars(S(CoreMeetHolder)) /\ WF_vars(S(CoreReconcile))
  /\ \A t \in Turns : WF_vars(S(CoreClaim(t))) /\ WF_vars(CoreReprobe(t))
  /\ WF_vars(S(CoreKillIdle)) /\ WF_vars(S(CoreAck)) /\ WF_vars(S(CoreKill)) /\ WF_vars(S(CoreGone)) /\ WF_vars(S(CoreReapExt))
  /\ WF_vars(S(TWStart)) /\ WF_vars(S(TWLock)) /\ WF_vars(S(TWExit)) /\ WF_vars(S(CoreReapTW))
  /\ WF_vars(S(HUp)) /\ WF_vars(S(\E m \in msgs : HRecv(m))) /\ WF_vars(S(HTryLock)) /\ WF_vars(S(HStart))
  /\ WF_vars(S(EngTurnEnd)) /\ WF_vars(S(OrphanTurnEnd)) /\ WF_vars(S(EngDie)) /\ WF_vars(S(StubDie))
  /\ \A d \in Directs : WF_vars(S(DLock(d))) /\ WF_vars(S(DReg(d))) /\ WF_vars(S(DExit(d)))

Spec == Init /\ [][Next]_vars /\ Fairness

---------------------------------------------------------------------------
Writers == (IF eng = "turn" THEN 1 ELSE 0)
         + Cardinality({d \in Directs : dSt[d] = "running"})
         + (IF twSt = "running" THEN 1 ELSE 0)

P1_NoTwoWriters == Writers <= 1
\* a turn that is claimed/running has the claim; nothing else ever drops a turn
P2_NoLostTurn == \A t \in Turns :
                   /\ st[t] \in {"new", "queued", "claimed", "running", "parked", "done", "cancelled"}
                   /\ (st[t] \in {"claimed", "running"} => claim = t)
P3_Liveness == \A t \in Turns : (st[t] = "queued") ~> (st[t] \in {"done", "cancelled"})
P4_GoneMeansEmpty == goneSeen => eng = "none"
P5_StaleNeverPrompted == ~staleP
P6_LockFileNeverDeleted == unlinks = 0
\* M3-0a (Q2): a terminal that registered through the core never runs beside a warm process of the conversation.
P7_NoWarmBesideRegisteredTerminal == \A d \in Directs : (dSt[d] = "running" /\ ext = d) => GroupEmpty

TypeOK ==
  /\ cst \in {"none", "claimed", "running", "ended", "parked"}
  /\ eng \in {"none", "idle", "turn"} /\ stub \in {"none", "up"}
  /\ lk \subseteq ({"holder", "stub", "eng", "child", "tw"} \cup Directs)
=============================================================================
