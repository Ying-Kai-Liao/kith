---------------------------- MODULE WinKeeper ----------------------------
(***************************************************************************)
(* W12 rule 4 re-run (2026-11-27). WinClaims.tla (../) assumed a named     *)
(* job could be opened by name after a host restart. W8's test showed the  *)
(* name lives only while some process holds a handle to the job: when the  *)
(* last handle closes the name leaves the namespace, though the job's      *)
(* processes run on. This model counts handles. Who holds the detached     *)
(* run's job J_k is a constant Mode:                                       *)
(*   "name"   the 2026-11-24 rule 4: the host makes named J_k (no kill on  *)
(*            close), holds it, and after a restart opens it by name;      *)
(*            "not found" reads as gone.                                   *)
(*   "keeper" the 2026-11-26 recommendation: the host makes named J_k (no  *)
(*            kill on close) and hands a handle to the holder, which keeps *)
(*            it until the job has no processes; "not found" is gone only  *)
(*            while the recorded holder instance lives, else not gone.     *)
(*   "holder" the holder makes J_k itself (unnamed, kill on close, only    *)
(*            the holder holds it) and creates the launcher in it; the     *)
(*            claim records the holder instance; the probe asks that       *)
(*            instance, and a dead instance means the job died with it.    *)
(*   "launcher" the launcher L (W8's winstart) makes J_k itself with kill  *)
(*            on close and is its only holder; L runs outside J_k and      *)
(*            starts E inside it in one call. When L dies, by any route,   *)
(*            J_k's last handle closes and E dies with it. The claim       *)
(*            records L; the probe reads L's pid and start time only.      *)
(* Everything else is WinClaims' design (rules 2, 3, 5-8, 10 as constants; *)
(* the launcher is always created inside its job in one call).            *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS MaxRuns, MaxRestarts, MaxKills, MaxStops, MaxProbeErr,
          MaxHolderCrashes, MaxHostTurns, MaxOpens,
          Mode,                \* "name" | "keeper" | "holder" | "launcher"
          LauncherKoc,         \* launcher: J_k kills on close (W8 as merged makes it with no limits)
          KeeperBeforeLaunch,  \* keeper: the holder has its handle before the launcher is created
          CloseAfterActive,    \* keeper: the handle closes on ACTIVE_PROCESS_ZERO only (the job had a process first)
          SoleHandle,          \* holder: nobody but the holder ever holds J_k (no copy kept by the host)
          UnknownNotGone,      \* any probe error (open, query, the holder's pipe) answers "not gone"
          NoBreakaway,         \* rule 3
          HostJobKillOnClose,  \* rule 6
          HolderKillOnClose,   \* rule 7
          NoInheritJobHandle,  \* rule 8
          ReportToEnding,      \* rule 5 (S4 R3a)
          TornMarkerCounts,    \* rule 10
          WriteBeforeSend,     \* premise P-W12a
          NameFirst,           \* keeper: the probe opens the name first, then checks the recorded keeper
          ExpireUnacked,       \* the holder drops an exit no core has acked after a bound (today: FALSE, holder.rs:648-663)
          SweepBeforeRecord,   \* the holder sweeps the tree before it records the exit (today: TRUE, holder.rs:1217-1221)
          OS,                  \* "unix" | "windows": whether the holder's own exit kills what its jobs still hold
          MaxCoreDowns, CoreComesBack

K == 1..MaxRuns
LiveC == {"claimed", "running", "ending"}
FinalT == {"done", "failed", "cancelled"}
LUp == {"up", "attached"}

VARIABLES
  req, delivered, nRuns, runLive, turn, redeemed,          \* the request and its runs (S4)
  claim, why, rep, probe, pmid, rinst,                     \* run claims; pmid = the probe's first read; rinst = holder instance on the claim
  lSt, lHow, eSt, eHow, cSt, cIn, sent, line, cancel, termd, \* L, E (both always in J_k), tool child C
  jobMade, hostH, keepH, koc, everAct, handed, sepoch, rleak, \* J_k and who holds a handle to it; rleak = a run process inherited one
  epoch, kills, stops, perr,
  hSt, hClaim, hLeak, hTurns,                              \* host-door turn
  tSt, tClaim, tLeak, holder, hcrash, opens, hinst,        \* held terminal, the holder and its instance
  texit, core, cdowns                                      \* the holder's unacked exit record; the core link

runV   == <<req, delivered, nRuns, runLive, turn, redeemed>>
claimV == <<claim, why, rep, probe, pmid, rinst>>
procV  == <<lSt, lHow, eSt, eHow, cSt, cIn, sent, line, cancel, termd>>
jobV   == <<jobMade, hostH, keepH, koc, everAct, handed, sepoch, rleak>>
ctrV   == <<epoch, kills, stops, perr>>
hostV  == <<hSt, hClaim, hLeak, hTurns>>
termV  == <<tSt, tClaim, tLeak, holder, hcrash, opens, hinst, texit, core, cdowns>>
vars   == <<runV, claimV, procV, jobV, ctrV, hostV, termV>>

Init ==
  /\ req = "owed" /\ delivered = 0 /\ nRuns = 0
  /\ runLive = [k \in K |-> FALSE] /\ turn = [k \in K |-> "none"] /\ redeemed = [k \in K |-> FALSE]
  /\ claim = [k \in K |-> "none"] /\ why = [k \in K |-> "none"]
  /\ rep = [k \in K |-> FALSE] /\ probe = [k \in K |-> "none"] /\ pmid = [k \in K |-> "none"]
  /\ rinst = [k \in K |-> 0]
  /\ lSt = [k \in K |-> "none"] /\ lHow = [k \in K |-> "none"]
  /\ eSt = [k \in K |-> "none"] /\ eHow = [k \in K |-> "none"]
  /\ cSt = [k \in K |-> "none"] /\ cIn = [k \in K |-> FALSE]
  /\ sent = [k \in K |-> FALSE] /\ line = [k \in K |-> "none"]
  /\ cancel = [k \in K |-> FALSE] /\ termd = [k \in K |-> FALSE]
  /\ jobMade = [k \in K |-> FALSE] /\ hostH = [k \in K |-> FALSE] /\ keepH = [k \in K |-> 0]
  /\ koc = [k \in K |-> FALSE] /\ everAct = [k \in K |-> FALSE] /\ handed = [k \in K |-> FALSE]
  /\ sepoch = [k \in K |-> 0] /\ rleak = [k \in K |-> FALSE]
  /\ epoch = 0 /\ kills = 0 /\ stops = 0 /\ perr = 0
  /\ hSt = "none" /\ hClaim = "none" /\ hLeak = FALSE /\ hTurns = 0
  /\ tSt = "none" /\ tClaim = "none" /\ tLeak = FALSE /\ holder = "up" /\ hcrash = 0 /\ opens = 0
  /\ hinst = 1 /\ texit = "none" /\ core = "up" /\ cdowns = 0

(* ---- helpers ---- *)
NoLiveClaim == tClaim = "none" /\ hClaim # "running" /\ \A j \in K : claim[j] \notin LiveC
PidAlive(k) == lSt[k] \in LUp
JobActive(k) == jobMade[k] /\ ((Mode # "launcher" /\ lSt[k] \in LUp) \/ eSt[k] = "alive" \/ (cIn[k] /\ cSt[k] = "alive"))
RunAlive(k) == lSt[k] \in LUp \/ eSt[k] = "alive" \/ cSt[k] = "alive"
RunWriter(k) == eSt[k] = "alive" \/ cSt[k] = "alive"
Writers == Cardinality({k \in K : RunWriter(k)})
           + (IF hSt = "alive" THEN 1 ELSE 0) + (IF tSt \in {"alive", "left"} THEN 1 ELSE 0)
HostHas(k) == hostH[k] /\ sepoch[k] = epoch
HolderLive(i) == i # 0 /\ holder = "up" /\ hinst = i
Named == Mode # "holder"
\* OpenJobObject by name finds the job only while a handle to it is open
NameExists(k) == Named /\ jobMade[k] /\ (hostH[k] \/ keepH[k] # 0)
MarkerSeen(k) == line[k] = "full" \/ (TornMarkerCounts /\ line[k] = "torn")

\* recorded_gone's honest answer: the launcher's pid + start time first, then the job
ProbeSays(k) ==
  IF PidAlive(k) THEN "alive"
  ELSE IF Mode = "name" THEN
         IF NameExists(k) THEN (IF JobActive(k) THEN "alive" ELSE "gone")
         ELSE "gone"                                   \* "not found" read as gone
  ELSE IF Mode = "keeper" THEN
         IF NameExists(k) THEN (IF JobActive(k) THEN "alive" ELSE "gone")
         ELSE IF HolderLive(rinst[k]) THEN "gone"      \* not found while the recorded keeper lives
         ELSE "notgone"                                \* the keeper changed: nobody can say
  ELSE IF Mode = "launcher" THEN "gone"                 \* L is gone: its kill-on-close job went with it
  ELSE \* holder
         IF HolderLive(rinst[k]) THEN (IF JobActive(k) THEN "alive" ELSE "gone")  \* the holder answers from its handle
         ELSE "gone"                                   \* that instance is dead: its kill-on-close job died with it
SaysGone(a) == a = "gone" \/ (a = "unknown" /\ ~UnknownNotGone)

\* processes of the jobs in D die (TerminateJobObject or kill-on-close firing)
KillL(D) == [k \in K |-> IF D[k] /\ lSt[k] \in LUp THEN "dead" ELSE lSt[k]]
KillLHow(D) == [k \in K |-> IF D[k] /\ lSt[k] \in LUp THEN "exit" ELSE lHow[k]]
KillE(D) == [k \in K |-> IF D[k] /\ eSt[k] = "alive" THEN "dead" ELSE eSt[k]]
KillEHow(D) == [k \in K |-> IF D[k] /\ eSt[k] = "alive" THEN "killed" ELSE eHow[k]]
KillC(D) == [k \in K |-> IF D[k] /\ cIn[k] /\ cSt[k] = "alive" THEN "dead" ELSE cSt[k]]

(* ---- the core: take and settle (S4 R4) ---- *)
TakeNext ==
  /\ req = "owed" /\ \A j \in K : ~runLive[j] /\ nRuns < MaxRuns
  /\ LET k == nRuns + 1 IN
       /\ nRuns' = k
       /\ runLive' = [runLive EXCEPT ![k] = TRUE]
       /\ turn' = [turn EXCEPT ![k] = "queued"]
  /\ req' = "inrun"
  /\ UNCHANGED <<delivered, redeemed, claimV, procV, jobV, ctrV, hostV, termV>>

Settle(k) ==
  /\ runLive[k] /\ turn[k] \in FinalT
  /\ req' = IF turn[k] = "done" \/ (redeemed[k] /\ MarkerSeen(k)) THEN "answered" ELSE "owed"
  /\ runLive' = [runLive EXCEPT ![k] = FALSE]
  /\ UNCHANGED <<delivered, nRuns, turn, redeemed, claimV, procV, jobV, ctrV, hostV, termV>>

ClaimRun(k) ==
  /\ turn[k] = "queued"
  /\ IF NoLiveClaim
       THEN /\ claim' = [claim EXCEPT ![k] = "claimed"]
            /\ turn' = [turn EXCEPT ![k] = "claimed"]
       ELSE /\ turn' = [turn EXCEPT ![k] = "cancelled"]
            /\ UNCHANGED claim
  /\ UNCHANGED <<req, delivered, nRuns, runLive, redeemed, why, rep, probe, pmid, rinst, procV, jobV, ctrV, hostV, termV>>

(* ---- the spawn, per Mode ---- *)
\* name: the host makes named J_k and L inside it in one call
SpawnName(k) ==
  /\ Mode = "name" /\ claim[k] = "claimed" /\ ~jobMade[k]
  /\ jobMade' = [jobMade EXCEPT ![k] = TRUE] /\ hostH' = [hostH EXCEPT ![k] = TRUE]
  /\ sepoch' = [sepoch EXCEPT ![k] = epoch] /\ everAct' = [everAct EXCEPT ![k] = TRUE]
  /\ lSt' = [lSt EXCEPT ![k] = "up"]
  /\ UNCHANGED <<runV, claimV, lHow, eSt, eHow, cSt, cIn, sent, line, cancel, termd>>
  /\ UNCHANGED <<keepH, koc, handed, rleak, ctrV, hostV, termV>>

\* keeper: the host makes named J_k; the claim records the keeper instance it will hand the job to
KMake(k) ==
  /\ Mode = "keeper" /\ claim[k] = "claimed" /\ ~jobMade[k] /\ holder = "up"
  /\ jobMade' = [jobMade EXCEPT ![k] = TRUE] /\ hostH' = [hostH EXCEPT ![k] = TRUE]
  /\ sepoch' = [sepoch EXCEPT ![k] = epoch] /\ rinst' = [rinst EXCEPT ![k] = hinst]
  /\ UNCHANGED <<runV, claim, why, rep, probe, pmid, procV, keepH, koc, everAct, handed, rleak, ctrV, hostV, termV>>

\* DuplicateHandle into the holder, which acknowledges
KHandover(k) ==
  /\ Mode = "keeper" /\ HostHas(k) /\ ~handed[k] /\ HolderLive(rinst[k])
  /\ keepH' = [keepH EXCEPT ![k] = hinst] /\ handed' = [handed EXCEPT ![k] = TRUE]
  /\ UNCHANGED <<runV, claimV, procV, jobMade, hostH, koc, everAct, sepoch, rleak, ctrV, hostV, termV>>

KLaunch(k) ==
  /\ Mode = "keeper" /\ HostHas(k) /\ lSt[k] = "none" /\ claim[k] = "claimed"
  /\ KeeperBeforeLaunch => handed[k]
  /\ lSt' = [lSt EXCEPT ![k] = "up"] /\ everAct' = [everAct EXCEPT ![k] = TRUE]
  /\ UNCHANGED <<runV, claimV, lHow, eSt, eHow, cSt, cIn, sent, line, cancel, termd>>
  /\ UNCHANGED <<jobMade, hostH, keepH, koc, handed, sepoch, rleak, ctrV, hostV, termV>>

\* holder: the host asks the holder; the holder makes J_k (kill on close) and L in it in one handler.
\* ~SoleHandle: the host keeps a copy of the handle too (say, to stop the run itself).
SpawnHolder(k) ==
  /\ Mode = "holder" /\ claim[k] = "claimed" /\ ~jobMade[k] /\ holder = "up"
  /\ jobMade' = [jobMade EXCEPT ![k] = TRUE] /\ koc' = [koc EXCEPT ![k] = TRUE]
  /\ keepH' = [keepH EXCEPT ![k] = hinst] /\ rinst' = [rinst EXCEPT ![k] = hinst]
  /\ hostH' = [hostH EXCEPT ![k] = ~SoleHandle] /\ sepoch' = [sepoch EXCEPT ![k] = epoch]
  /\ everAct' = [everAct EXCEPT ![k] = TRUE] /\ handed' = [handed EXCEPT ![k] = TRUE]
  /\ rleak' \in {rleak} \cup (IF NoInheritJobHandle THEN {} ELSE {[rleak EXCEPT ![k] = TRUE]})
  /\ lSt' = [lSt EXCEPT ![k] = "up"]
  /\ UNCHANGED <<runV, claim, why, rep, probe, pmid, lHow, eSt, eHow, cSt, cIn, sent, line, cancel, termd>>
  /\ UNCHANGED <<ctrV, hostV, termV>>

\* the keeper (keeper or holder mode) closes its handle once the job is empty
KeeperClose(k) ==
  /\ Mode \in {"keeper", "holder"} /\ keepH[k] # 0 /\ HolderLive(keepH[k]) /\ ~JobActive(k)
  /\ CloseAfterActive => everAct[k]
  /\ keepH' = [keepH EXCEPT ![k] = 0]
  /\ UNCHANGED <<runV, claimV, procV, jobMade, hostH, koc, everAct, handed, sepoch, rleak, ctrV, hostV, termV>>

\* launcher: the host starts L (in no job of the run's); L makes J_k when it starts E
SpawnL(k) ==
  /\ Mode = "launcher" /\ claim[k] = "claimed" /\ lSt[k] = "none"
  /\ lSt' = [lSt EXCEPT ![k] = "up"] /\ sepoch' = [sepoch EXCEPT ![k] = epoch]
  /\ UNCHANGED <<runV, claimV, lHow, eSt, eHow, cSt, cIn, sent, line, cancel, termd>>
  /\ UNCHANGED <<jobMade, hostH, keepH, koc, everAct, handed, rleak, ctrV, hostV, termV>>

\* launcher: the jobs whose processes die when L dies (J_k's only handle was L's)
LDeath(k) == [j \in K |-> Mode = "launcher" /\ j = k /\ jobMade[k] /\ koc[k] /\ ~rleak[k]]

(* ---- the launcher L ---- *)
Attach(k) ==
  /\ lSt[k] = "up"
  /\ IF claim[k] = "claimed"
       THEN /\ lSt' = [lSt EXCEPT ![k] = "attached"]
            /\ claim' = [claim EXCEPT ![k] = "running"]
            /\ turn' = [turn EXCEPT ![k] = "running"]
            /\ redeemed' = [redeemed EXCEPT ![k] = TRUE]
            /\ UNCHANGED lHow
       ELSE /\ lSt' = [lSt EXCEPT ![k] = "dead"]
            /\ lHow' = [lHow EXCEPT ![k] = "exit"]
            /\ UNCHANGED <<claim, turn, redeemed>>
  /\ UNCHANGED <<req, delivered, nRuns, runLive, why, rep, probe, pmid, rinst>>
  /\ UNCHANGED <<eSt, eHow, cSt, cIn, sent, line, cancel, termd, jobV, ctrV, hostV, termV>>

StartEngine(k) ==
  /\ lSt[k] = "attached" /\ eSt[k] = "none"
  /\ eSt' = [eSt EXCEPT ![k] = "alive"]
  /\ IF Mode = "launcher"
       THEN /\ jobMade' = [jobMade EXCEPT ![k] = TRUE] /\ koc' = [koc EXCEPT ![k] = LauncherKoc]
            /\ everAct' = [everAct EXCEPT ![k] = TRUE]
            /\ rleak' \in {rleak} \cup (IF NoInheritJobHandle THEN {} ELSE {[rleak EXCEPT ![k] = TRUE]})
       ELSE UNCHANGED <<jobMade, koc, everAct, rleak>>
  /\ UNCHANGED <<runV, claimV, lSt, lHow, eHow, cSt, cIn, sent, line, cancel, termd>>
  /\ UNCHANGED <<hostH, keepH, handed, sepoch, ctrV, hostV, termV>>

LExit(k) ==
  /\ lSt[k] = "attached" /\ eSt[k] # "alive"
  /\ lSt' = [lSt EXCEPT ![k] = "dead"]
  /\ lHow' = [lHow EXCEPT ![k] = IF eHow[k] = "done" THEN "done" ELSE "exit"]
  /\ cSt' = KillC(LDeath(k))
  /\ UNCHANGED <<runV, claimV, eSt, eHow, cIn, sent, line, cancel, termd, jobV, ctrV, hostV, termV>>

LauncherKilledAlone(k) ==
  /\ lSt[k] \in LUp /\ kills < MaxKills
  /\ lSt' = [lSt EXCEPT ![k] = "dead"]
  /\ lHow' = [lHow EXCEPT ![k] = "exit"]
  /\ eSt' = KillE(LDeath(k)) /\ eHow' = KillEHow(LDeath(k)) /\ cSt' = KillC(LDeath(k))
  /\ kills' = kills + 1
  /\ UNCHANGED <<runV, claimV, cIn, sent, line, cancel, termd, jobV>>
  /\ UNCHANGED <<epoch, stops, perr, hostV, termV>>

(* ---- the engine E and its tool child C ---- *)
Send(k) ==
  /\ eSt[k] = "alive" /\ ~sent[k]
  /\ sent' = [sent EXCEPT ![k] = TRUE]
  /\ line' = [line EXCEPT ![k] = IF WriteBeforeSend THEN "torn" ELSE "none"]
  /\ delivered' = delivered + 1
  /\ UNCHANGED <<req, nRuns, runLive, turn, redeemed, claimV>>
  /\ UNCHANGED <<lSt, lHow, eSt, eHow, cSt, cIn, cancel, termd, jobV, ctrV, hostV, termV>>

LineStart(k) ==
  /\ ~WriteBeforeSend /\ eSt[k] = "alive" /\ sent[k] /\ line[k] = "none"
  /\ line' = [line EXCEPT ![k] = "torn"]
  /\ UNCHANGED <<runV, claimV, lSt, lHow, eSt, eHow, cSt, cIn, sent, cancel, termd, jobV, ctrV, hostV, termV>>

LineDone(k) ==
  /\ eSt[k] = "alive" /\ line[k] = "torn"
  /\ line' = [line EXCEPT ![k] = "full"]
  /\ UNCHANGED <<runV, claimV, lSt, lHow, eSt, eHow, cSt, cIn, sent, cancel, termd, jobV, ctrV, hostV, termV>>

EExit(k) ==
  /\ eSt[k] = "alive"
  /\ eSt' = [eSt EXCEPT ![k] = "dead"]
  /\ line' = [line EXCEPT ![k] = IF sent[k] THEN "full" ELSE line[k]]
  /\ eHow' = [eHow EXCEPT ![k] = IF sent[k] THEN "done" ELSE "exit"]
  /\ UNCHANGED <<runV, claimV, lSt, lHow, cSt, cIn, sent, cancel, termd, jobV, ctrV, hostV, termV>>

ECrash(k) ==
  /\ eSt[k] = "alive" /\ kills < MaxKills
  /\ eSt' = [eSt EXCEPT ![k] = "dead"]
  /\ eHow' = [eHow EXCEPT ![k] = "killed"]
  /\ kills' = kills + 1
  /\ UNCHANGED <<runV, claimV, lSt, lHow, cSt, cIn, sent, line, cancel, termd, jobV>>
  /\ UNCHANGED <<epoch, stops, perr, hostV, termV>>

SpawnChild(k) ==
  /\ eSt[k] = "alive" /\ cSt[k] = "none"
  /\ cSt' = [cSt EXCEPT ![k] = "alive"]
  /\ \E b \in (IF NoBreakaway THEN {TRUE} ELSE BOOLEAN) : cIn' = [cIn EXCEPT ![k] = b]
  /\ UNCHANGED <<runV, claimV, lSt, lHow, eSt, eHow, sent, line, cancel, termd, jobV, ctrV, hostV, termV>>

CExit(k) ==
  /\ cSt[k] = "alive"
  /\ cSt' = [cSt EXCEPT ![k] = "dead"]
  /\ UNCHANGED <<runV, claimV, lSt, lHow, eSt, eHow, cIn, sent, line, cancel, termd, jobV, ctrV, hostV, termV>>

(* ---- Stop (D6): cancel, then TerminateJobObject through whoever holds J_k ---- *)
Cancel(k) ==
  /\ eSt[k] = "alive" /\ ~cancel[k] /\ stops < MaxStops
  /\ cancel' = [cancel EXCEPT ![k] = TRUE]
  /\ stops' = stops + 1
  /\ UNCHANGED <<runV, claimV, lSt, lHow, eSt, eHow, cSt, cIn, sent, line, termd, jobV>>
  /\ UNCHANGED <<epoch, kills, perr, hostV, termV>>

CanTerm(k) == IF Mode = "holder" THEN HolderLive(rinst[k]) /\ keepH[k] = rinst[k]
              ELSE IF Mode = "launcher" THEN jobMade[k] /\ PidAlive(k)   \* L terminates its job, or a Stop kills L
              ELSE NameExists(k)

TerminateJob(k) ==
  /\ cancel[k] /\ ~termd[k] /\ CanTerm(k)
  /\ termd' = [termd EXCEPT ![k] = TRUE]
  /\ LET D == [j \in K |-> j = k] IN
       /\ lSt' = KillL(D) /\ lHow' = KillLHow(D) /\ eSt' = KillE(D) /\ eHow' = KillEHow(D) /\ cSt' = KillC(D)
  /\ UNCHANGED <<runV, claimV, cIn, sent, line, cancel, jobV, ctrV, hostV, termV>>

(* ---- L's exit is reported by whoever spawned it and still waits on it ---- *)
Waiter(k) == IF Mode = "holder" THEN HolderLive(rinst[k]) ELSE sepoch[k] = epoch

Report(k) ==
  /\ lSt[k] = "dead" /\ ~rep[k] /\ (jobMade[k] \/ Mode = "launcher") /\ Waiter(k)
  /\ rep' = [rep EXCEPT ![k] = TRUE]
  /\ IF claim[k] = "running"
       THEN IF ReportToEnding
              THEN /\ claim' = [claim EXCEPT ![k] = "ending"]
                   /\ why' = [why EXCEPT ![k] = lHow[k]]
                   /\ UNCHANGED turn
              ELSE /\ claim' = [claim EXCEPT ![k] = "ended"]
                   /\ turn' = [turn EXCEPT ![k] = IF lHow[k] = "done" THEN "done" ELSE "failed"]
                   /\ UNCHANGED why
       ELSE UNCHANGED <<claim, why, turn>>
  /\ UNCHANGED <<req, delivered, nRuns, runLive, redeemed, probe, pmid, rinst, procV, jobV, ctrV, hostV, termV>>

(* ---- end_gone: probe outside the tx, then the CAS ---- *)
\* keeper mode reads in two steps (open by name, check the recorded keeper), in the order NameFirst says;
\* the other modes read in one step (a pid check, or one question to the holder answered from its handle)
NameRead(k) == IF PidAlive(k) THEN "alive"
               ELSE IF NameExists(k) THEN (IF JobActive(k) THEN "alive" ELSE "gone")
               ELSE "nf"
ProbeRead(k) ==
  /\ claim[k] \in {"running", "ending"} /\ probe[k] = "none"
  /\ \/ /\ perr < MaxProbeErr
        /\ probe' = [probe EXCEPT ![k] = "unknown"] /\ pmid' = [pmid EXCEPT ![k] = "none"]
        /\ perr' = perr + 1
     \/ /\ UNCHANGED perr
        /\ IF Mode # "keeper"
             THEN /\ probe' = [probe EXCEPT ![k] = ProbeSays(k)] /\ UNCHANGED pmid
           ELSE IF NameFirst
             THEN IF pmid[k] = "none"
                    THEN IF NameRead(k) = "nf"
                           THEN /\ pmid' = [pmid EXCEPT ![k] = "nf"] /\ UNCHANGED probe
                           ELSE /\ probe' = [probe EXCEPT ![k] = NameRead(k)] /\ UNCHANGED pmid
                    ELSE /\ probe' = [probe EXCEPT ![k] = IF HolderLive(rinst[k]) THEN "gone" ELSE "notgone"]
                         /\ pmid' = [pmid EXCEPT ![k] = "none"]
           ELSE IF pmid[k] = "none"
                    THEN /\ pmid' = [pmid EXCEPT ![k] = IF HolderLive(rinst[k]) THEN "hl" ELSE "hd"]
                         /\ UNCHANGED probe
                    ELSE /\ probe' = [probe EXCEPT ![k] =
                                IF NameRead(k) # "nf" THEN NameRead(k)
                                ELSE IF pmid[k] = "hl" THEN "gone" ELSE "notgone"]
                         /\ pmid' = [pmid EXCEPT ![k] = "none"]
  /\ UNCHANGED <<runV, claim, why, rep, rinst, procV, jobV, epoch, kills, stops, hostV, termV>>

EndGoneCAS(k) ==
  /\ probe[k] # "none"
  /\ probe' = [probe EXCEPT ![k] = "none"]
  /\ IF claim[k] \in {"running", "ending"} /\ SaysGone(probe[k])
       THEN /\ claim' = [claim EXCEPT ![k] = "ended"]
            /\ turn' = [turn EXCEPT ![k] = IF claim[k] = "ending" /\ why[k] = "done" THEN "done" ELSE "failed"]
       ELSE UNCHANGED <<claim, turn>>
  /\ UNCHANGED <<req, delivered, nRuns, runLive, redeemed, why, rep, pmid, rinst, procV, jobV, ctrV, hostV, termV>>

(* ---- host-door turn (rule 6) ---- *)
HostTurnStart ==
  /\ hSt = "none" /\ hTurns < MaxHostTurns /\ NoLiveClaim
  /\ hSt' = "alive" /\ hClaim' = "running" /\ hTurns' = hTurns + 1
  /\ hLeak' \in (IF NoInheritJobHandle THEN {FALSE} ELSE BOOLEAN)
  /\ UNCHANGED <<runV, claimV, procV, jobV, ctrV, termV>>

HostTurnFinish ==
  /\ hSt = "alive"
  /\ hSt' = "dead"
  /\ hClaim' = IF hClaim = "running" THEN "ended" ELSE hClaim
  /\ UNCHANGED <<hLeak, hTurns, runV, claimV, procV, jobV, ctrV, termV>>

(* ---- the host restarts: its handles close (a kill-on-close job it held last dies) ---- *)
Restart ==
  /\ epoch < MaxRestarts
  /\ LET D == [k \in K |-> koc[k] /\ hostH[k] /\ keepH[k] = 0 /\ ~rleak[k]] IN
       /\ lSt' = KillL(D) /\ lHow' = KillLHow(D) /\ eSt' = KillE(D) /\ eHow' = KillEHow(D) /\ cSt' = KillC(D)
  /\ hostH' = [k \in K |-> FALSE]
  /\ epoch' = epoch + 1
  /\ claim' = [k \in K |-> IF claim[k] = "claimed" THEN "ended" ELSE claim[k]]
  /\ turn' = [k \in K |-> IF claim[k] = "claimed" THEN "cancelled" ELSE turn[k]]
  /\ hClaim' = IF hClaim = "running" THEN "ended" ELSE hClaim
  /\ hSt' = IF hSt = "alive" /\ HostJobKillOnClose /\ ~hLeak THEN "dead" ELSE hSt
  /\ UNCHANGED <<req, delivered, nRuns, runLive, redeemed, why, rep, probe, pmid, rinst>>
  /\ UNCHANGED <<cIn, sent, line, cancel, termd, jobMade, keepH, koc, everAct, handed, sepoch, rleak>>
  /\ UNCHANGED <<kills, stops, perr, hLeak, hTurns, termV>>

(* ---- the held terminal ----
   tSt: "alive" = T runs; "left" = T's leader ended but a child it started still runs (only before a sweep);
   "dead" = the whole tree is gone. texit: the exit record the holder keeps until a core acks it (holder.rs:648-663). *)
TOther == <<runV, claimV, procV, jobV, ctrV, hostV>>
TermOpen ==
  /\ tSt = "none" /\ tClaim = "none" /\ holder = "up" /\ core = "up" /\ opens < MaxOpens /\ NoLiveClaim
  /\ tClaim' = "live" /\ tSt' = "alive" /\ opens' = opens + 1
  /\ tLeak' \in (IF NoInheritJobHandle THEN {FALSE} ELSE BOOLEAN)
  /\ UNCHANGED <<holder, hcrash, hinst, texit, core, cdowns, TOther>>

\* T's leader ends. The holder (if up) sweeps the tree and records the exit; with ~SweepBeforeRecord it records first.
TermExit ==
  /\ tSt = "alive"
  /\ IF holder = "up"
       THEN /\ texit' = "unacked"
            /\ tSt' \in (IF SweepBeforeRecord THEN {"dead"} ELSE {"dead", "left"})
       ELSE /\ tSt' \in {"dead", "left"} /\ UNCHANGED texit   \* nobody sweeps (only after a leak let T outlive its holder)
  /\ UNCHANGED <<tClaim, tLeak, holder, hcrash, opens, hinst, core, cdowns, TOther>>

\* a broken holder's later sweep, or the child ending on its own
TermLeftEnds ==
  /\ tSt = "left"
  /\ tSt' = "dead"
  /\ UNCHANGED <<tClaim, tLeak, holder, hcrash, opens, hinst, texit, core, cdowns, TOther>>

\* the core acks the exit: the holder forgets it, the core ends the claim
TermAck ==
  /\ holder = "up" /\ core = "up" /\ tClaim = "live" /\ texit = "unacked"
  /\ tClaim' = "none" /\ texit' = "none"
  /\ UNCHANGED <<tSt, tLeak, holder, hcrash, opens, hinst, core, cdowns, TOther>>

\* the core reconciles with the same holder instance: a claim the holder no longer lists is ended as unlisted
\* (holder_link.rs:186-188). The holder lists a running leader or an exit record.
TermUnlisted ==
  /\ holder = "up" /\ core = "up" /\ tClaim = "live" /\ texit = "none" /\ tSt # "alive"
  /\ tClaim' = "none"
  /\ UNCHANGED <<tSt, tLeak, holder, hcrash, opens, hinst, texit, core, cdowns, TOther>>

\* the fix under question: an exit nobody acked is dropped after a bound
Expire ==
  /\ ExpireUnacked /\ holder = "up" /\ texit = "unacked"
  /\ texit' = "none"
  /\ UNCHANGED <<tSt, tClaim, tLeak, holder, hcrash, opens, hinst, core, cdowns, TOther>>

\* the core goes away (a stop, an uninstall); with ~CoreComesBack it never returns
CoreDown ==
  /\ core = "up" /\ cdowns < MaxCoreDowns
  /\ core' = "down" /\ cdowns' = cdowns + 1
  /\ UNCHANGED <<tSt, tClaim, tLeak, holder, hcrash, opens, hinst, texit, TOther>>
CoreUp ==
  /\ core = "down" /\ CoreComesBack
  /\ core' = "up"
  /\ UNCHANGED <<tSt, tClaim, tLeak, holder, hcrash, opens, hinst, texit, cdowns, TOther>>

\* watch_idle (holder.rs:352-366): no core, nothing listed, then exit. Its handles close: on Windows a kill-on-close job
\* whose only handle it held takes what is left in it; on Unix a leftover child lives on.
HolderIdleExit ==
  /\ holder = "up" /\ core = "down" /\ texit = "none" /\ tSt # "alive" /\ \A k \in K : keepH[k] = 0
  /\ holder' = "down"
  /\ tSt' = IF tSt = "left" /\ OS = "windows" /\ HolderKillOnClose /\ ~tLeak THEN "dead" ELSE tSt
  /\ UNCHANGED <<tClaim, tLeak, hcrash, opens, hinst, texit, core, cdowns, TOther>>

(* ---- the holder crashes: every handle it held closes; a kill-on-close job it held last dies ---- *)
HolderCrash ==
  /\ holder = "up" /\ hcrash < MaxHolderCrashes
  /\ holder' = "down" /\ hcrash' = hcrash + 1 /\ texit' = "none"
  /\ tSt' = IF tSt \in {"alive", "left"} /\ HolderKillOnClose /\ ~tLeak THEN "dead" ELSE tSt
  /\ LET D == [k \in K |-> koc[k] /\ keepH[k] # 0 /\ ~hostH[k] /\ ~rleak[k]] IN
       /\ lSt' = KillL(D) /\ lHow' = KillLHow(D) /\ eSt' = KillE(D) /\ eHow' = KillEHow(D) /\ cSt' = KillC(D)
  /\ keepH' = [k \in K |-> 0]
  /\ UNCHANGED <<tClaim, tLeak, opens, hinst, core, cdowns, runV, claimV, cIn, sent, line, cancel, termd>>
  /\ UNCHANGED <<jobMade, hostH, koc, everAct, handed, sepoch, rleak, ctrV, hostV>>

\* a new holder instance says hello to a core; the old instance's terminal claim is released (end_groups is empty on
\* Windows; on Unix it kills the old instance's groups, so a leftover child there dies here)
NewHolder ==
  /\ holder = "down" /\ core = "up"
  /\ holder' = "up" /\ hinst' = hinst + 1 /\ tClaim' = "none"
  /\ tSt' = IF tSt = "left" /\ OS = "unix" THEN "dead" ELSE tSt
  /\ UNCHANGED <<tLeak, hcrash, opens, texit, core, cdowns, TOther>>

Next ==
  \/ TakeNext \/ Restart
  \/ HostTurnStart \/ HostTurnFinish
  \/ TermOpen \/ TermExit \/ TermLeftEnds \/ TermAck \/ TermUnlisted \/ Expire \/ CoreDown \/ CoreUp
  \/ HolderIdleExit \/ HolderCrash \/ NewHolder
  \/ \E k \in K :
       \/ Settle(k) \/ ClaimRun(k)
       \/ SpawnName(k) \/ SpawnL(k) \/ KMake(k) \/ KHandover(k) \/ KLaunch(k) \/ SpawnHolder(k) \/ KeeperClose(k)
       \/ Attach(k) \/ StartEngine(k) \/ LExit(k) \/ LauncherKilledAlone(k)
       \/ Send(k) \/ LineStart(k) \/ LineDone(k) \/ EExit(k) \/ ECrash(k) \/ SpawnChild(k) \/ CExit(k)
       \/ Cancel(k) \/ TerminateJob(k) \/ Report(k) \/ ProbeRead(k) \/ EndGoneCAS(k)

Spec == Init /\ [][Next]_vars
FairSpec == Spec /\ \A k \in K : WF_vars(ProbeRead(k)) /\ WF_vars(EndGoneCAS(k))
IdleSpec == Spec /\ WF_vars(Expire) /\ WF_vars(HolderIdleExit) /\ WF_vars(TermLeftEnds)

(* ---- properties ---- *)
OneWriter == Writers <= 1
ExactlyOnce == delivered <= 1
AnsweredDelivered == req = "answered" => delivered >= 1
NoEndWhileLive == \A k \in K : (claim[k] = "ended" /\ redeemed[k]) => ~RunAlive(k)
\* a claim whose run is really over is ended in the end (no "stuck until reboot")
NotStuck == \A k \in K : (claim[k] \in {"running", "ending"} /\ ~RunAlive(k)) ~> (claim[k] = "ended")
\* with the core gone for good and nothing running in the holder, the holder ends in the end (no "stays until reboot")
HolderIdles == (core = "down" /\ ~CoreComesBack /\ tSt \in {"none", "dead"} /\ \A k \in K : keepH[k] = 0)
               ~> (holder = "down")
=============================================================================
