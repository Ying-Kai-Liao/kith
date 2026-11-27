--------------------------- MODULE HolderIdleGen ---------------------------
(* PR #1251 review (2026-11-27). A focused model of one held terminal's claim across holder link drops, core restarts and
   holder restarts, with the core's reconcile as the code runs it, not as WinKeeper's TermUnlisted had it:
   - step 5 (open_claims, holder_link.rs ~802-818): the first hello of a core GENERATION ends the claims the welcome does
     not list; a second hello in the same generation does nothing (claims::is_reconciled);
   - step 7 (reconcile, holder_link.rs ~869): a hello from another holder INSTANCE than the recorded one ends the old
     instance's unlisted claims (different_holder);
   - end_gone (claims.rs ~1026) reaps only 'ending' or 'external' claims, so a running 'terminal' claim is ended only by an
     exit report (EXITED/ACK) or by step 5 / step 7.
   The tree is swept before the exit is recorded (holder.rs reap), so T is dead whenever texit = "unacked".
   Mode: "today" (no expiry), "drop" (PR #1251: drop an unacked exit while no core is linked), "exit" (alternative: keep
   the record and let the holder instance end once the bound passes with no core linked). The bound is not timed here:
   the holder may act at any moment while the link is down. *)
EXTENDS Naturals

CONSTANTS Mode, MaxLinkDrops, MaxCoreRestarts, CoreComesBack

VARIABLES
  tSt,      \* "none" | "alive" | "dead"
  tClaim,   \* "none" | "live": the core's terminal claim for T
  cInst,    \* the holder instance on T's claim
  texit,    \* "none" | "unacked": the holder's exit record
  holder,   \* "up" | "down"
  hinst,    \* the holder's instance number (a new one at every start)
  core,     \* "up" | "down": the core process
  link,     \* "up" | "down"
  gen,      \* the core's generation (bumped at every core start)
  recGen,   \* the generation whose step 5 has run
  recInst,  \* the holder instance in the core's record
  drops, restarts

vars == <<tSt, tClaim, cInst, texit, holder, hinst, core, link, gen, recGen, recInst, drops, restarts>>

Init ==
  /\ tSt = "none" /\ tClaim = "none" /\ cInst = 0 /\ texit = "none"
  /\ holder = "up" /\ hinst = 1 /\ core = "up" /\ link = "up"
  /\ gen = 1 /\ recGen = 1 /\ recInst = 1 /\ drops = 0 /\ restarts = 0

\* what the holder's welcome lists for T: its own process while it runs, or its exit record
Listed == holder = "up" /\ cInst = hinst /\ (tSt = "alive" \/ texit = "unacked")

Open ==
  /\ tSt = "none" /\ tClaim = "none" /\ link = "up"
  /\ tSt' = "alive" /\ tClaim' = "live" /\ cInst' = hinst
  /\ UNCHANGED <<texit, holder, hinst, core, link, gen, recGen, recInst, drops, restarts>>

\* T ends; the holder sweeps its tree, then records the exit (and sends EXITED, which only a linked core reads)
TermExit ==
  /\ tSt = "alive" /\ holder = "up"
  /\ tSt' = "dead" /\ texit' = "unacked"
  /\ UNCHANGED <<tClaim, cInst, holder, hinst, core, link, gen, recGen, recInst, drops, restarts>>

\* a linked core reads the exit (EXITED or the welcome), ends the claim and acks; the holder forgets the record
Ack ==
  /\ link = "up" /\ texit = "unacked" /\ holder = "up"
  /\ texit' = "none"
  /\ tClaim' = IF tClaim = "live" /\ cInst = hinst THEN "none" ELSE tClaim
  /\ UNCHANGED <<tSt, cInst, holder, hinst, core, link, gen, recGen, recInst, drops, restarts>>

\* the link drops with the core still running (same generation)
LinkDrop ==
  /\ link = "up" /\ drops < MaxLinkDrops
  /\ link' = "down" /\ drops' = drops + 1
  /\ UNCHANGED <<tSt, tClaim, cInst, texit, holder, hinst, core, gen, recGen, recInst, restarts>>

CoreStop ==
  /\ core = "up" /\ restarts < MaxCoreRestarts
  /\ core' = "down" /\ link' = "down" /\ restarts' = restarts + 1
  /\ UNCHANGED <<tSt, tClaim, cInst, texit, holder, hinst, gen, recGen, recInst, drops>>

CoreStart ==
  /\ core = "down" /\ CoreComesBack
  /\ core' = "up" /\ gen' = gen + 1
  /\ UNCHANGED <<tSt, tClaim, cInst, texit, holder, hinst, link, recGen, recInst, drops, restarts>>

\* the core's hello: it starts a holder when none answers (a new instance), then runs step 7 and step 5 as the code does
Hello ==
  /\ core = "up" /\ link = "down"
  /\ LET up   == TRUE
         inst == IF holder = "up" THEN hinst ELSE hinst + 1
         listed == holder = "up" /\ cInst = inst /\ (tSt = "alive" \/ texit = "unacked")
         step7 == recInst # inst /\ tClaim = "live" /\ cInst = recInst /\ ~listed
         step5 == recGen # gen /\ tClaim = "live" /\ ~listed
     IN /\ holder' = "up" /\ hinst' = inst
        /\ link' = "up"
        /\ tClaim' = IF step7 \/ step5 THEN "none" ELSE tClaim
        /\ recInst' = inst /\ recGen' = gen
  /\ UNCHANGED <<tSt, cInst, texit, core, gen, drops, restarts>>

\* PR #1251: drop an unacked exit while no core is linked
Drop ==
  /\ Mode = "drop" /\ holder = "up" /\ link = "down" /\ texit = "unacked"
  /\ texit' = "none"
  /\ UNCHANGED <<tSt, tClaim, cInst, holder, hinst, core, link, gen, recGen, recInst, drops, restarts>>

\* watch_idle: no core, nothing running; today and "drop" only with no record, "exit" also with exit records
IdleExit ==
  /\ holder = "up" /\ link = "down" /\ tSt # "alive"
  /\ (texit = "none" \/ Mode = "exit")
  /\ holder' = "down" /\ texit' = "none"
  /\ UNCHANGED <<tSt, tClaim, cInst, hinst, core, link, gen, recGen, recInst, drops, restarts>>

Next == Open \/ TermExit \/ Ack \/ LinkDrop \/ CoreStop \/ CoreStart \/ Hello \/ Drop \/ IdleExit

Spec == Init /\ [][Next]_vars
  /\ WF_vars(TermExit) /\ WF_vars(Ack) /\ WF_vars(CoreStart) /\ WF_vars(Hello)
  /\ WF_vars(Drop) /\ WF_vars(IdleExit)

\* a dead terminal's claim is ended once a core is linked again
ClaimEnds == (tClaim = "live" /\ tSt = "dead") ~> (tClaim = "none")
\* with the core gone for good, the holder ends
HolderIdles == (core = "down" /\ ~CoreComesBack) ~> (holder = "down")
=============================================================================
