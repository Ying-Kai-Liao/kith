---------------------------- MODULE ReconcileOwed ----------------------------
\* PR #1020's Reconcile.tla (f4ce7a6fb: renames, the reconcile by current id) plus the fix for its finding 1
\* (design 2026-11-17-rename-failcut). A revoke whose read of the ids fails cuts only the id given, and after a rename
\* the host's rows are under the old one. Fix "owed cut": when cut_off_held's read of the ids fails (held() errs), the
\* host records owed(person) durably; the authority poll, under AUTH_WRITES, when held() answers, cuts every id and
\* clears the row; an undo clears the row in its own locked commit. Code: serve/host/src/remote/core_revokes.rs
\* (owe_cut, pay_owed, clear_owed), api/people.rs (cut_off_held, local_revoke), remote/members.rs (apply_revoked).
\* Checks the DESIGN, not the code. Rows: reconcile-owed.tsv.
EXTENDS Naturals, TLC

CONSTANTS MaxOps, SplitRead, RestoreToo, Renames, RecByCurrent,
          Owed,          \* the fix: a cut whose ids read failed leaves an owed row, retried by the poll
          UndoKeepsOwed, \* broken: the undo does not clear the owed row
          UndoBlind,     \* an undo's read of the ids may fail, so its clear misses a row kept under a former id
          UndoMarks      \* the fix: such an undo leaves a marker (a newer sequence number) the retry reads

VARIABLES rev, hs, app, seen, rec, failcut, termundo, ops, renamed, missed, owed, mark

vars == <<rev, hs, app, seen, rec, failcut, termundo, ops, renamed, missed, owed, mark>>

Init == rev = FALSE /\ hs = FALSE /\ app = "idle" /\ seen = FALSE /\ rec = "idle"
        /\ failcut = FALSE /\ termundo = FALSE /\ ops = 0 /\ renamed = FALSE /\ missed = FALSE /\ owed = FALSE /\ mark = FALSE

Budget == ops < MaxOps
Reaches(ok) == ~renamed \/ ok
RecSees(x) == x /\ (~renamed \/ ~RecByCurrent)

Rename == Renames /\ ~renamed /\ renamed' = TRUE
          /\ UNCHANGED <<rev, hs, app, seen, rec, failcut, termundo, ops, missed, owed, mark>>

TermRevoke == Budget /\ ~rev /\ rev' = TRUE /\ ops' = ops + 1
              /\ UNCHANGED <<hs, app, seen, rec, failcut, termundo, renamed, missed, owed, mark>>
TermUndo   == Budget /\ rev /\ rev' = FALSE /\ termundo' = TRUE /\ ops' = ops + 1
              /\ UNCHANGED <<hs, app, seen, rec, failcut, renamed, missed, owed, mark>>

AppRevoke  == Budget /\ app = "idle" /\ rec = "idle" /\ rev' = TRUE /\ app' = "cut" /\ ops' = ops + 1
              /\ UNCHANGED <<hs, seen, rec, failcut, termundo, renamed, missed, owed, mark>>
AppRevokeFails == Budget /\ app = "idle" /\ rec = "idle" /\ app' = "cut" /\ failcut' = TRUE /\ ops' = ops + 1
              /\ UNCHANGED <<rev, hs, seen, rec, termundo, renamed, missed, owed, mark>>
\* ok: held() answered. The code cannot tell whether a rename happened, so a failed read always leaves the row.
AppCut     == app = "cut" /\ app' = "idle"
              /\ \E ok \in BOOLEAN : /\ hs' = (hs \/ Reaches(ok))
                                    /\ owed' = (owed \/ (Owed /\ ~ok))
                                    \* a row written now is newer than any marker
                                    /\ mark' = (mark /\ ~(Owed /\ ~ok))
              /\ UNCHANGED <<rev, seen, rec, failcut, termundo, ops, renamed, missed>>
AppUndo    == Budget /\ app = "idle" /\ rec = "idle" /\ rev' = FALSE /\ app' = "restore" /\ ops' = ops + 1
              /\ \E blind \in BOOLEAN :
                   IF UndoBlind /\ blind
                     THEN owed' = owed /\ mark' = (mark \/ UndoMarks)   \* the clear missed the row; the marker is written
                     ELSE owed' = (owed /\ UndoKeepsOwed) /\ mark' = mark
              /\ UNCHANGED <<hs, seen, rec, failcut, termundo, renamed, missed>>
AppRestore == app = "restore" /\ failcut' = FALSE /\ termundo' = FALSE /\ app' = "idle"
              /\ \E ok \in BOOLEAN : /\ hs' = (hs /\ ~Reaches(ok))
                                    /\ missed' = (hs /\ ~Reaches(ok))
              /\ UNCHANGED <<rev, seen, rec, ops, renamed, owed, mark>>

RecAtomic  == ~SplitRead /\ rec = "idle"
              /\ hs' = (IF RestoreToo THEN RecSees(rev) ELSE (hs \/ RecSees(rev)))
              /\ UNCHANGED <<rev, app, seen, rec, failcut, termundo, ops, renamed, missed, owed, mark>>
RecRead    == SplitRead /\ rec = "idle" /\ seen' = rev /\ rec' = "cut"
              /\ UNCHANGED <<rev, hs, app, failcut, termundo, ops, renamed, missed, owed, mark>>
RecCut     == SplitRead /\ rec = "cut"
              /\ hs' = (IF RestoreToo THEN RecSees(seen) ELSE (hs \/ RecSees(seen))) /\ rec' = "idle"
              /\ UNCHANGED <<rev, app, seen, failcut, termundo, ops, renamed, missed, owed, mark>>

\* The poll pays the owed cut: under AUTH_WRITES (atomic), only on a read of the ids that answered (a failed read is a
\* stutter), cut every id, drop the row.
\* A row older than an undo marker is dropped unpaid (the person was undone after it); otherwise paid.
OwedRetry  == owed /\ rec = "idle" /\ owed' = FALSE /\ mark' = FALSE /\ hs' = (hs \/ ~mark)
              /\ UNCHANGED <<rev, app, seen, rec, failcut, termundo, ops, renamed, missed>>

Next == TermRevoke \/ TermUndo \/ AppRevoke \/ AppRevokeFails \/ AppCut \/ AppUndo \/ AppRestore
        \/ RecAtomic \/ RecRead \/ RecCut \/ Rename \/ OwedRetry

Spec == Init /\ [][Next]_vars /\ WF_vars(AppCut) /\ WF_vars(AppRestore)
        /\ WF_vars(RecAtomic) /\ WF_vars(RecRead) /\ WF_vars(RecCut) /\ WF_vars(OwedRetry)

Idle == app = "idle" /\ rec = "idle"
NoLostUndo == (Idle /\ ~rev /\ hs) => (failcut \/ termundo \/ missed)
FailCutHolds == (failcut /\ app = "idle") => hs
\* With the fix: the fail-closed cut holds, or is owed and will be paid.
FailCutHoldsOwed == (failcut /\ app = "idle") => (hs \/ owed)
FailCutReached == [](failcut => <>(hs \/ ~failcut))
Narrows == [](rev => <>(hs \/ ~rev))
TypeOK == rev \in BOOLEAN /\ hs \in BOOLEAN /\ app \in {"idle", "cut", "restore"} /\ rec \in {"idle", "cut"}
          /\ renamed \in BOOLEAN /\ missed \in BOOLEAN /\ owed \in BOOLEAN /\ mark \in BOOLEAN
=============================================================================
