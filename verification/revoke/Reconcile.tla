---------------------------- MODULE Reconcile ----------------------------
\* The host's reconcile of a core-side revoke on a switched Mac (design 2026-11-13, migrate blockers, point 3).
\* Checks the DESIGN, not the code.
\*
\* rev:  the core says the person is revoked (people.suspend / people.undo committed).
\* hs:   the host has set the person aside (store.suspend_person: devices, memberships, invites kept for undo).
\* App revoke/undo (local_revoke) is two steps: the core commit under AUTH_WRITES, then cut_off / restored() after
\* the lock is let go. A failed core write on a revoke still runs cut_off (fail closed), with rev unchanged.
\* A terminal `kith kernel core-call people.suspend|undo` commits in the core and tells the host nothing.
\* Reconcile (the design): under AUTH_WRITES, read the core's people, and for a person the core shows revoked whom the
\* host has not set aside, set them aside. Never the other way (narrowing only).
EXTENDS Naturals, TLC

CONSTANTS MaxOps, SplitRead, RestoreToo

VARIABLES rev, hs, app, seen, rec, failcut, termundo, ops

vars == <<rev, hs, app, seen, rec, failcut, termundo, ops>>

Init == rev = FALSE /\ hs = FALSE /\ app = "idle" /\ seen = FALSE /\ rec = "idle"
        /\ failcut = FALSE /\ termundo = FALSE /\ ops = 0

Budget == ops < MaxOps

\* --- the terminal: core only --------------------------------------------------------------------------------------
TermRevoke == Budget /\ ~rev /\ rev' = TRUE /\ ops' = ops + 1
              /\ UNCHANGED <<hs, app, seen, rec, failcut, termundo>>
TermUndo   == Budget /\ rev /\ rev' = FALSE /\ termundo' = TRUE /\ ops' = ops + 1
              /\ UNCHANGED <<hs, app, seen, rec, failcut>>

\* --- the app: commit under the lock, then the host's half outside it ----------------------------------------------
\* The lock is modelled by atomicity: a commit is one step, and the reconcile (design) is one step.
AppRevoke  == Budget /\ app = "idle" /\ rec = "idle" /\ rev' = TRUE /\ app' = "cut" /\ ops' = ops + 1
              /\ UNCHANGED <<hs, seen, rec, failcut, termundo>>
AppRevokeFails == Budget /\ app = "idle" /\ rec = "idle" /\ app' = "cut" /\ failcut' = TRUE /\ ops' = ops + 1
              /\ UNCHANGED <<rev, hs, seen, rec, termundo>>
AppCut     == app = "cut" /\ hs' = TRUE /\ app' = "idle"
              /\ UNCHANGED <<rev, seen, rec, failcut, termundo, ops>>
AppUndo    == Budget /\ app = "idle" /\ rec = "idle" /\ rev' = FALSE /\ app' = "restore" /\ ops' = ops + 1
              /\ UNCHANGED <<hs, seen, rec, failcut, termundo>>
AppRestore == app = "restore" /\ hs' = FALSE /\ failcut' = FALSE /\ termundo' = FALSE /\ app' = "idle"
              /\ UNCHANGED <<rev, seen, rec, ops>>

\* --- the reconcile -------------------------------------------------------------------------------------------------
\* Design: read and set aside in one step under AUTH_WRITES (the app's commits take the same lock; a commit is atomic
\* here, so the step can't fall inside one).
RecAtomic  == ~SplitRead /\ rec = "idle"
              /\ hs' = (IF RestoreToo THEN rev ELSE (hs \/ rev))
              /\ UNCHANGED <<rev, app, seen, rec, failcut, termundo, ops>>
\* Broken: the read is taken outside the lock, the set-aside comes later.
RecRead    == SplitRead /\ rec = "idle" /\ seen' = rev /\ rec' = "cut"
              /\ UNCHANGED <<rev, hs, app, failcut, termundo, ops>>
RecCut     == SplitRead /\ rec = "cut"
              /\ hs' = (IF RestoreToo THEN seen ELSE (hs \/ seen)) /\ rec' = "idle"
              /\ UNCHANGED <<rev, app, seen, failcut, termundo, ops>>

Next == TermRevoke \/ TermUndo \/ AppRevoke \/ AppRevokeFails \/ AppCut \/ AppUndo \/ AppRestore
        \/ RecAtomic \/ RecRead \/ RecCut

Spec == Init /\ [][Next]_vars /\ WF_vars(AppCut) /\ WF_vars(AppRestore)
        /\ WF_vars(RecAtomic) /\ WF_vars(RecRead) /\ WF_vars(RecCut)

\* --- properties ----------------------------------------------------------------------------------------------------
Idle == app = "idle" /\ rec = "idle"

\* No lost undo: once nothing is in flight, the host holds a person set aside whom the core has back only for a reason
\* the design accepts: a revoke whose core write failed (fail closed until the owner acts), or an undo made only in the
\* core (the owner then undoes in the app, which is idempotent in the core).
NoLostUndo == (Idle /\ ~rev /\ hs) => (failcut \/ termundo)

\* A fail-closed cut stays until the owner acts: the reconcile never gives back what the app cut.
FailCutHolds == (failcut /\ app = "idle") => hs

\* Narrowing reaches the host: a core revoke is followed by the host setting the person aside (or by an undo).
Narrows == [](rev => <>(hs \/ ~rev))

TypeOK == rev \in BOOLEAN /\ hs \in BOOLEAN /\ app \in {"idle", "cut", "restore"} /\ rec \in {"idle", "cut"}
=============================================================================
