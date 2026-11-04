---- MODULE Holder ----
\* Copied unchanged from the architect: /Users/Kath/kith/agents/architect/files/kith/designs/2026-11-04-merged-fencing-holder/model/Holder.tla (2026-11-04).
\* The architect left no cfgs for it; the holder-*.cfg files here are M3-0a's, one per flag combination in checks.tsv.
\* core/holder at rust-rebuild 73a03656 (M0d #663/#669) and the core's side in core/runtime/src/holder_link.rs (M0c),
\* one conversation. Modelled from the code:
\*   holder.rs spawn: if an entry with the same (conv, claim) is held, answer it; else start a process, new entry with a
\*     fresh id (ids restart at 1 per holder instance). Generation fencing and superseded connections are left out:
\*     a frame from an older connection is refused under the state lock, so the core's view is one FIFO connection.
\*   reap: the exit is recorded once and kept until an ACK by id; ACK and KILL carry only the id; EXITED only the id.
\*   holder crash: entries are memory, so they go; on Unix the processes live on (orphans).
\*   holder_link: the core records the running pids on hello and on each 5 s poll; on meeting a new instance it ends
\*     the recorded pids of the old one (step 7), then records the new list.
\* The core's claim logic is M3 and not written yet. It is modelled as the design says (take the next claim only once
\* the current one is over; after a new instance, release the current claim once step 7 has run), plus the faults
\* the design must survive: a reply lost with its connection, a core crash, and (StaleSends) a late spawn for a claim
\* that is no longer current.
EXTENDS Naturals, FiniteSets

CONSTANTS MaxClaim, MaxInst, MaxPid, MaxGen,
          StaleSends,          \* the core may send a spawn for a claim that is no longer current
          RefuseOlder,         \* planned fix 1: refuse a claim below the highest one held for the conversation
          HighWater,           \* stronger: refuse a claim at or below the highest one ever started, kept after the ack
          RefuseWhileRunning,  \* refuse a spawn while another claim's process of the conversation still runs
          KeyByInstance,       \* planned fix 2: the core keys holder ids by (instance, id)
          RecordAtSpawn,       \* the core records a pid when it hears SPAWNED (not only on the poll)
          WriterLock,          \* the M3 stub: holds the conversation's lock for the engine's life; busy = exit
          RedeemChecksClaim,   \* the M3 stub's redeem refuses a claim that is not core.db's current one (with WriterLock)
          HolderCrash,         \* the holder may crash (one restart per instance bound)
          \* M3-2 (core/runtime/src/conversations/claims.rs). RedeemWrite on = the rules of the skill's "Open finding": the
          \* redeem is a core.db CAS that records the redeemer's pid, a release on a new instance is a CAS, and a running
          \* claim is never released until the recorded pid is gone. Off = the omniscient core of the architect's model
          \* (the claim is only "done or not"), which fails DoneTruth after a holder crash.
          RedeemWrite,
          RedeemCAS,           \* (with RedeemWrite) the stub starts its engine only if the redeem CAS succeeds
          HoldRunning          \* (with RedeemWrite) an unlisted running claim goes to ending, not straight to ended

Pids == 1..MaxPid
Max(a, b) == IF a > b THEN a ELSE b

VARIABLES inst, entries, nextId, hw,
          pState, pClaim, pInst, nextPid,
          gen, conn, cur, cst, cpid, idmap, heard, rec, recInst, writers

vars == <<inst, entries, nextId, hw, pState, pClaim, pInst, nextPid,
          gen, conn, cur, cst, cpid, idmap, heard, rec, recInst, writers>>

Init ==
  /\ inst = 1 /\ entries = {} /\ nextId = 1 /\ hw = 0
  /\ pState = [p \in Pids |-> "none"] /\ pClaim = [p \in Pids |-> 0] /\ pInst = [p \in Pids |-> 0]
  /\ nextPid = 1
  /\ gen = 1 /\ conn = FALSE /\ cur = 0 /\ cst = "ended" /\ cpid = 0
  /\ idmap = {} /\ heard = {} /\ rec = {} /\ recInst = 0
  /\ writers = {}

Live(p) == pState[p] \in {"starting", "writer"}

\* core.db's claims.state of the current claim (claim 0 = none yet = over): claimed -> running (redeem) -> ending (a new
\* instance does not list it) -> ended (the recorded pid is gone); claimed -> ended (release, an exit report).
\* cpid is claims.pid, the redeemer's process.
curDone == cst = "ended"

\* What the core takes holder id `id` (heard on instance i) to be.
MapsTo(i, id, k) == \E m \in idmap : m.id = id /\ m.claim = k /\ (KeyByInstance => m.inst = i)

\* ---- the core ----

\* Hello + WELCOME (holder_link connect/connected/reconcile).
Connect ==
  /\ ~conn
  /\ LET newInst == recInst # 0 /\ recInst # inst
         listed  == {e.pid : e \in entries}
         killed  == IF newInst THEN {p \in rec : pInst[p] = recInst /\ p \notin listed} ELSE {}
         unlisted == newInst /\ ~\E e \in entries : e.claim = cur IN
     /\ conn' = TRUE
     /\ idmap' = idmap \cup {[inst |-> inst, id |-> e.id, claim |-> e.claim] : e \in entries}
     /\ heard' = heard \cup {[inst |-> inst, id |-> e.id, pid |-> e.pid] : e \in {f \in entries : f.exit}}
     /\ pState' = [p \in Pids |-> IF p \in killed /\ Live(p) THEN "ended" ELSE pState[p]]
     /\ rec' = {e.pid : e \in {f \in entries : ~f.exit}}
     /\ recInst' = inst
     \* design s.4 step 7. claims.rs release_unlisted: an unlisted claimed claim is ended by CAS (a late redeem fails); an
     \* unlisted running one goes to ending and stays the writer until its recorded pid is gone (HoldRunning). Without
     \* RedeemWrite: the claim is released outright, once the old groups the core knew of are ended.
     /\ cst' = IF ~RedeemWrite THEN (IF curDone \/ unlisted THEN "ended" ELSE cst)
              ELSE IF unlisted /\ cst = "claimed" THEN "ended"
              ELSE IF unlisted /\ cst = "running" THEN (IF HoldRunning THEN "ending" ELSE "ended")
              ELSE cst
  /\ UNCHANGED <<inst, entries, nextId, hw, pClaim, pInst, nextPid, gen, cur, cpid, writers>>

Disconnect ==
  /\ conn /\ conn' = FALSE
  /\ UNCHANGED <<inst, entries, nextId, hw, pState, pClaim, pInst, nextPid, gen, cur, cst, cpid, idmap, heard, rec, recInst, writers>>

\* The core crashes and restarts: memory gone, core.db (cur, cst, cpid, rec, recInst) kept, generation bumped.
CoreRestart ==
  /\ gen < MaxGen
  /\ gen' = gen + 1 /\ conn' = FALSE /\ idmap' = {} /\ heard' = {}
  /\ UNCHANGED <<inst, entries, nextId, hw, pState, pClaim, pInst, nextPid, cur, cst, cpid, rec, recInst, writers>>

TakeClaim ==
  /\ curDone /\ cur < MaxClaim
  /\ cur' = cur + 1 /\ cst' = "claimed" /\ cpid' = 0
  /\ UNCHANGED <<inst, entries, nextId, hw, pState, pClaim, pInst, nextPid, gen, conn, idmap, heard, rec, recInst, writers>>

\* The holder's spawn decision for claim k (holder.rs spawn plus the switches).
Refused(k) ==
  \/ RefuseOlder /\ \E e \in entries : e.claim > k
  \/ HighWater /\ k <= hw
  \/ RefuseWhileRunning /\ \E e \in entries : e.claim # k /\ ~e.exit

\* The core sends SPAWN(k); the holder handles it under its lock; the reply may be lost with the connection.
Spawn(k, lost) ==
  /\ conn
  /\ \/ k = cur /\ ~curDone
     \/ StaleSends /\ k >= 1 /\ (k < cur \/ (k = cur /\ curDone))
  /\ IF \E e \in entries : e.claim = k
       THEN LET e == CHOOSE f \in entries : f.claim = k IN
            /\ idmap' = IF lost THEN idmap ELSE idmap \cup {[inst |-> inst, id |-> e.id, claim |-> k]}
            /\ rec' = IF RecordAtSpawn /\ ~lost /\ ~e.exit THEN rec \cup {e.pid} ELSE rec
            /\ UNCHANGED <<entries, nextId, hw, pState, pClaim, pInst, nextPid, writers>>
       ELSE IF Refused(k)
       THEN UNCHANGED <<entries, nextId, hw, pState, pClaim, pInst, nextPid, writers, rec, idmap>>
       ELSE /\ nextPid <= MaxPid
            /\ entries' = entries \cup {[id |-> nextId, claim |-> k, pid |-> nextPid, exit |-> FALSE]}
            /\ nextId' = nextId + 1 /\ nextPid' = nextPid + 1
            /\ hw' = Max(hw, k)
            /\ pState' = [pState EXCEPT ![nextPid] = IF WriterLock THEN "starting" ELSE "writer"]
            /\ pClaim' = [pClaim EXCEPT ![nextPid] = k]
            /\ pInst' = [pInst EXCEPT ![nextPid] = inst]
            /\ writers' = IF WriterLock THEN writers ELSE writers \cup {[claim |-> k, pid |-> nextPid]}
            /\ idmap' = IF lost THEN idmap ELSE idmap \cup {[inst |-> inst, id |-> nextId, claim |-> k]}
            /\ rec' = IF RecordAtSpawn /\ ~lost THEN rec \cup {nextPid} ELSE rec
  /\ conn' = ~lost
  /\ UNCHANGED <<inst, gen, cur, cst, cpid, heard, recInst>>

\* The 5 s poll: LIST, and the record follows the running entries.
Poll ==
  /\ conn /\ recInst = inst
  /\ rec' = {e.pid : e \in {f \in entries : ~f.exit}}
  /\ UNCHANGED <<inst, entries, nextId, hw, pState, pClaim, pInst, nextPid, gen, conn, cur, cst, cpid, idmap, heard, recInst, writers>>

\* EXITED reaches the core.
Hear(e) ==
  /\ conn /\ e \in entries /\ e.exit
  /\ heard' = heard \cup {[inst |-> inst, id |-> e.id, pid |-> e.pid]}
  /\ UNCHANGED <<inst, entries, nextId, hw, pState, pClaim, pInst, nextPid, gen, conn, cur, cst, cpid, idmap, rec, recInst, writers>>

\* The core takes a heard exit as the end of the claim it maps the id to (claims.rs report, kinds done and exit). With
\* RedeemWrite an exit that names another process than the recorded redeemer ends nothing, and so does one that names none (apply: the pid must equal `rec`). In the code a nameless one moves a claim with a recorded pid to `ending`; it stays the writer and only `ConfirmGone` (end_gone) ends it, so the model has no step for it (a stutter).
Settle(h) ==
  /\ h \in heard /\ ~curDone /\ MapsTo(h.inst, h.id, cur)
  /\ (RedeemWrite /\ cst \in {"running", "ending"}) => h.pid = cpid
  /\ cst' = "ended"
  /\ UNCHANGED <<inst, entries, nextId, hw, pState, pClaim, pInst, nextPid, gen, conn, cur, cpid, idmap, heard, rec, recInst, writers>>

\* claims.rs end_gone (after the kill by recorded pid and start time): ending -> ended only once the pid is confirmed gone.
\* The kill itself needs no holder connection: it is by pid.
KillRecorded ==
  /\ RedeemWrite /\ cst = "ending" /\ cpid # 0
  /\ pState' = [pState EXCEPT ![cpid] = IF Live(cpid) THEN "ended" ELSE pState[cpid]]
  /\ UNCHANGED <<inst, entries, nextId, hw, pClaim, pInst, nextPid, gen, conn, cur, cst, cpid, idmap, heard, rec, recInst, writers>>

ConfirmGone ==
  /\ RedeemWrite /\ cst = "ending" /\ cpid # 0 /\ ~Live(cpid)
  /\ cst' = "ended"
  /\ UNCHANGED <<inst, entries, nextId, hw, pState, pClaim, pInst, nextPid, gen, conn, cur, cpid, idmap, heard, rec, recInst, writers>>

\* ACK(id) for an exit the core heard; the holder drops the entry if it has an exit (holder.rs p::ACK).
Ack(h) ==
  /\ conn /\ h \in heard /\ (KeyByInstance => h.inst = inst)
  /\ entries' = {e \in entries : ~(e.id = h.id /\ e.exit)}
  /\ heard' = heard \ {h}
  /\ UNCHANGED <<inst, nextId, hw, pState, pClaim, pInst, nextPid, gen, conn, cur, cst, cpid, idmap, rec, recInst, writers>>

\* KILL(id) for the id the core maps to its current claim (a revoke or a take-over).
Kill(m) ==
  /\ conn /\ m \in idmap /\ m.claim = cur /\ ~curDone /\ (KeyByInstance => m.inst = inst)
  /\ pState' = [p \in Pids |-> IF (\E e \in entries : e.id = m.id /\ e.pid = p) /\ Live(p) THEN "ended" ELSE pState[p]]
  /\ UNCHANGED <<inst, entries, nextId, hw, pClaim, pInst, nextPid, gen, conn, cur, cst, cpid, idmap, heard, rec, recInst, writers>>

\* ---- processes and the holder ----

\* The M3 stub takes the conversation's writer lock, or exits busy.
\* With RedeemWrite, claims.rs redeem: the CAS claimed -> running for the stub's own claim, recording its pid (cpid); a
\* refused CAS (the claim was released, ended, or is another's) and the stub exits without starting the engine. Lock and
\* redeem are one step here: a stub that holds the lock between the two writes nothing.
\* Without RedeemCAS the stub starts its engine whatever the claim says (the record is only written from claimed).
TakeLock(p) ==
  /\ WriterLock /\ pState[p] = "starting"
  /\ LET casOk == pClaim[p] = cur /\ cst = "claimed"
         refused == IF RedeemWrite
                      THEN RedeemCAS /\ ~casOk
                      ELSE RedeemChecksClaim /\ (pClaim[p] # cur \/ curDone) IN
     IF \/ \E q \in Pids : pState[q] = "writer"
        \/ refused
       THEN /\ pState' = [pState EXCEPT ![p] = "ended"] /\ UNCHANGED <<writers, cst, cpid>>
       ELSE /\ pState' = [pState EXCEPT ![p] = "writer"] /\ writers' = writers \cup {[claim |-> pClaim[p], pid |-> p]}
            /\ IF RedeemWrite /\ casOk THEN cst' = "running" /\ cpid' = p ELSE UNCHANGED <<cst, cpid>>
  /\ UNCHANGED <<inst, entries, nextId, hw, pClaim, pInst, nextPid, gen, conn, cur, idmap, heard, rec, recInst>>

\* The job ends by itself (orphans too).
End(p) ==
  /\ pState[p] = "writer"
  /\ pState' = [pState EXCEPT ![p] = "ended"]
  /\ UNCHANGED <<inst, entries, nextId, hw, pClaim, pInst, nextPid, gen, conn, cur, cst, cpid, idmap, heard, rec, recInst, writers>>

\* reap: the exit is recorded once, after the tree is swept.
Reap(e) ==
  /\ e \in entries /\ ~e.exit /\ pState[e.pid] = "ended"
  /\ entries' = (entries \ {e}) \cup {[e EXCEPT !.exit = TRUE]}
  /\ UNCHANGED <<inst, nextId, hw, pState, pClaim, pInst, nextPid, gen, conn, cur, cst, cpid, idmap, heard, rec, recInst, writers>>

Crash ==
  /\ HolderCrash /\ inst < MaxInst
  /\ inst' = inst + 1 /\ entries' = {} /\ nextId' = 1 /\ hw' = 0 /\ conn' = FALSE
  /\ UNCHANGED <<pState, pClaim, pInst, nextPid, gen, cur, cst, cpid, idmap, heard, rec, recInst, writers>>

Next ==
  \/ Connect \/ Disconnect \/ CoreRestart \/ TakeClaim \/ Poll \/ Crash \/ KillRecorded \/ ConfirmGone
  \/ \E k \in 1..MaxClaim, lost \in BOOLEAN : Spawn(k, lost)
  \/ \E e \in entries : Hear(e) \/ Reap(e)
  \/ \E h \in heard : Settle(h) \/ Ack(h)
  \/ \E m \in idmap : Kill(m)
  \/ \E p \in Pids : TakeLock(p) \/ End(p)

Spec == Init /\ [][Next]_vars

\* At most one engine writes the conversation (I11).
OneWriter == Cardinality({p \in Pids : pState[p] = "writer"}) <= 1
\* A claim's job runs (becomes the writer) at most once.
RunOnce == \A w1, w2 \in writers : w1.claim = w2.claim => w1.pid = w2.pid
\* When the core takes its claim as over, no process of that claim still writes.
DoneTruth == curDone => \A p \in Pids : (pClaim[p] = cur /\ cur > 0) => pState[p] # "writer"
====
