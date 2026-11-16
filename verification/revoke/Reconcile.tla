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
\*
\* A RENAME in the core (R-rename, 2026-11-16; switch Renames): the host never moves its rows, so after it `hs` is about
\* the rows under the person's OLD id. Every host narrowing reads the person's ids (`core_revokes::held_under`: the
\* current id and every former one) and acts on each. The reconcile reads them in its own locked step (`to_cut`,
\* `host_ids`), so it always reaches the old id. The app's cut and undo (`cut_off_held`, the undo's loop over
\* `held_under`) read them AFTER the commit, and fall back to the given (current) id when the core can't say: a cut that
\* misses the old id is caught by the reconcile (Narrows); an undo that misses it leaves the old id set aside (fail
\* closed, `missed`: the owner undoes again). A former id is never handed out again, so cutting it reaches no one else.
EXTENDS Naturals, TLC

CONSTANTS MaxOps, SplitRead, RestoreToo,
          Renames,       \* the core may rename the person, once, at any time
          RecByCurrent   \* broken: the reconcile cuts by the current id alone (to_cut without `was`)

VARIABLES rev, hs, app, seen, rec, failcut, termundo, ops, renamed, missed

vars == <<rev, hs, app, seen, rec, failcut, termundo, ops, renamed, missed>>

Init == rev = FALSE /\ hs = FALSE /\ app = "idle" /\ seen = FALSE /\ rec = "idle"
        /\ failcut = FALSE /\ termundo = FALSE /\ ops = 0 /\ renamed = FALSE /\ missed = FALSE

Budget == ops < MaxOps

\* Whether a host step whose read of the ids answered (`ok`) reaches the host's rows: always, unless the person was
\* renamed and the read fell back to the current id.
Reaches(ok) == ~renamed \/ ok
\* What the reconcile's read reaches: every id (host_ids), or with RecByCurrent only the current one.
RecSees(x) == x /\ (~renamed \/ ~RecByCurrent)

\* --- the core's rename: the host's rows stay under the old id -------------------------------------------------------
Rename == Renames /\ ~renamed /\ renamed' = TRUE
          /\ UNCHANGED <<rev, hs, app, seen, rec, failcut, termundo, ops, missed>>

\* --- the terminal: core only --------------------------------------------------------------------------------------
TermRevoke == Budget /\ ~rev /\ rev' = TRUE /\ ops' = ops + 1
              /\ UNCHANGED <<hs, app, seen, rec, failcut, termundo, renamed, missed>>
TermUndo   == Budget /\ rev /\ rev' = FALSE /\ termundo' = TRUE /\ ops' = ops + 1
              /\ UNCHANGED <<hs, app, seen, rec, failcut, renamed, missed>>

\* --- the app: commit under the lock, then the host's half outside it ----------------------------------------------
\* The lock is modelled by atomicity: a commit is one step, and the reconcile (design) is one step.
AppRevoke  == Budget /\ app = "idle" /\ rec = "idle" /\ rev' = TRUE /\ app' = "cut" /\ ops' = ops + 1
              /\ UNCHANGED <<hs, seen, rec, failcut, termundo, renamed, missed>>
AppRevokeFails == Budget /\ app = "idle" /\ rec = "idle" /\ app' = "cut" /\ failcut' = TRUE /\ ops' = ops + 1
              /\ UNCHANGED <<rev, hs, seen, rec, termundo, renamed, missed>>
AppCut     == app = "cut" /\ app' = "idle"
              /\ \E ok \in BOOLEAN : hs' = (hs \/ Reaches(ok))
              /\ UNCHANGED <<rev, seen, rec, failcut, termundo, ops, renamed, missed>>
AppUndo    == Budget /\ app = "idle" /\ rec = "idle" /\ rev' = FALSE /\ app' = "restore" /\ ops' = ops + 1
              /\ UNCHANGED <<hs, seen, rec, failcut, termundo, renamed, missed>>
AppRestore == app = "restore" /\ failcut' = FALSE /\ termundo' = FALSE /\ app' = "idle"
              /\ \E ok \in BOOLEAN : /\ hs' = (hs /\ ~Reaches(ok))
                                    /\ missed' = (hs /\ ~Reaches(ok))
              /\ UNCHANGED <<rev, seen, rec, ops, renamed>>

\* --- the reconcile -------------------------------------------------------------------------------------------------
\* Design: read and set aside in one step under AUTH_WRITES (the app's commits take the same lock; a commit is atomic
\* here, so the step can't fall inside one).
RecAtomic  == ~SplitRead /\ rec = "idle"
              /\ hs' = (IF RestoreToo THEN RecSees(rev) ELSE (hs \/ RecSees(rev)))
              /\ UNCHANGED <<rev, app, seen, rec, failcut, termundo, ops, renamed, missed>>
\* Broken: the read is taken outside the lock, the set-aside comes later.
RecRead    == SplitRead /\ rec = "idle" /\ seen' = rev /\ rec' = "cut"
              /\ UNCHANGED <<rev, hs, app, failcut, termundo, ops, renamed, missed>>
RecCut     == SplitRead /\ rec = "cut"
              /\ hs' = (IF RestoreToo THEN RecSees(seen) ELSE (hs \/ RecSees(seen))) /\ rec' = "idle"
              /\ UNCHANGED <<rev, app, seen, failcut, termundo, ops, renamed, missed>>

Next == TermRevoke \/ TermUndo \/ AppRevoke \/ AppRevokeFails \/ AppCut \/ AppUndo \/ AppRestore
        \/ RecAtomic \/ RecRead \/ RecCut \/ Rename

Spec == Init /\ [][Next]_vars /\ WF_vars(AppCut) /\ WF_vars(AppRestore)
        /\ WF_vars(RecAtomic) /\ WF_vars(RecRead) /\ WF_vars(RecCut)

\* --- properties ----------------------------------------------------------------------------------------------------
Idle == app = "idle" /\ rec = "idle"

\* No lost undo: once nothing is in flight, the host holds a person set aside whom the core has back only for a reason
\* the design accepts: a revoke whose core write failed (fail closed until the owner acts), an undo made only in the
\* core (the owner then undoes in the app, which is idempotent in the core), or an undo after a rename whose read of
\* the ids fell back to the current one (`missed`: fail closed, the owner undoes again).
NoLostUndo == (Idle /\ ~rev /\ hs) => (failcut \/ termundo \/ missed)

\* The same without the rename's fallback: fails on purpose with Renames (row rename-undo-miss), the cost named above.
NoLostUndoStrict == (Idle /\ ~rev /\ hs) => (failcut \/ termundo)

\* A fail-closed cut stays until the owner acts: the reconcile never gives back what the app cut.
FailCutHolds == (failcut /\ app = "idle") => hs

\* Narrowing reaches the host: a core revoke is followed by the host setting the person aside (or by an undo). With a
\* rename, `hs` is the rows under the old id: a cut by the current id alone does not count.
Narrows == [](rev => <>(hs \/ ~rev))

TypeOK == rev \in BOOLEAN /\ hs \in BOOLEAN /\ app \in {"idle", "cut", "restore"} /\ rec \in {"idle", "cut"}
          /\ renamed \in BOOLEAN /\ missed \in BOOLEAN
=============================================================================
