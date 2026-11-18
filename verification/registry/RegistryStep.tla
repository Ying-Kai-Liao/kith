---- MODULE RegistryStep ----
\* The transcripts session-registry cutover (manager core-transcripts-2, 2026-11-17; the contract is the scratch
\* contract.md of that task, the architect's MUSTs 1-5 in arch1.txt). One session id (sid): its registry file
\* `$KITH_STATE/live/terminal/<sid>.json`, its `session_records` row in core.db, and its write-ahead spool entries
\* `live/terminal/inbox/<sid>.<recordedAt>.<pid>.json`.
\*
\* A record is named by its key: the time it was made (recordedAt in ns for a new hook; the file's mtime for an
\* old-kernel hook that writes no recordedAt). Keys come from one counter, so a larger key is a newer record. 0 = none.
\*
\* Actors:
\*   hooks     the session's SessionStart hooks (kernel/bin/kith-sessions record, kernel/lib/kith_registry.py), one
\*             after the other (one Claude process per sid; a hook ends or is killed before the next starts). A new
\*             hook reads the mode once: Files mode writes the file, drains the inbox, re-checks the mode; Writer
\*             mode spools first, calls session.record, deletes its own entry on an answer. Any hook can be killed
\*             at any step (Claude's 5 s limit). An "old" hook is a session pinned to an older kernel (MUST 1): it
\*             writes <sid>.json directly whatever the mode. A "bad" hook makes a record the core refuses as invalid.
\*   core      session.record (newer-wins txn, then the per-sid locked file write, answer after the rename), the
\*             60 s reconcile and inbox drain, the hourly prune; it can crash at any step (between commit and rename).
\*   kith-sched  in Files mode: the hourly prune_terminal and inbox drain.
\*   step      kith migrate step transcripts: preflight, import, tombstone, before_tombstone re-read, marker+switch;
\*             cancel; rollback (daemon stopped, export_back, marker removed).
\*
\* Readers read the file (Phase A); once switched the row is the truth and the file its projection.
EXTENDS Naturals, Integers, FiniteSets, Sequences

CONSTANTS K1, K2, K3,     \* the session's three hooks, in order: "new" | "old" | "bad"
          MaxCrash,       \* core crashes
          MaxPrune,       \* prunes (core or kith-sched)
          WriteAhead,     \* TRUE: spool, then call (MUST 3). FALSE: call, then spool on no answer
          OneName,        \* TRUE: one spool name per sid (inbox/<sid>.json), so entries replace and delete each other
          NsDrops,        \* TRUE: a not-serving answer drops the spool entry like an invalid one (MUST 5 broken)
          FileFrom,       \* "row": the core writes the file from the row read back (MUST 2); "request": from the request
          ProjectRule,    \* "reconcile": the file write after a record is the reconcile of that sid (a newer file is
                          \* taken into the row first); "row": write the row over the file whatever the file holds
          RowWrite,       \* "conditional": adopting a file into the row, or pruning a row, is decided inside the write
                          \* (newer-wins upsert; delete only the row judged), A7. "blind": from what the look read
          Reconcile,      \* TRUE: the daemon reconciles rows and files every 60 s (MUST 1); FALSE: never
          MissingKey,     \* order key of a file with no recordedAt: "mtime" (its mtime) | "oldest" (older than anything)
          ExportRule,     \* export_back writes a row to its file: "newer" (only over an older file) | "different"
          FileWrite       \* "atomic": the core's look at the file and its rename are one step (exchange-rename and
                          \* check what was swapped out); "blind": look, then a plain rename later (a writer that takes
                          \* no lock, an old hook, can write in between)

Kinds == <<K1, K2, K3>>
N == 3
Hooks == 1..N
MaxKey == 2 * N + 1
Keys == 0..MaxKey
Max(S) == CHOOSE x \in S : \A y \in S : y <= x
Max2(a, b) == IF a > b THEN a ELSE b

VARIABLES phase,      \* "files0" no step yet | "pre" stepping | "imp" imported | "tomb" tombstone written | "reread"
                      \* | "on" switched | "rb" rolling-back, daemon stopped | "rbx" exported | "files" after cancel/rollback
          file,       \* key of the record in <sid>.json, 0 = no file
          row,        \* key of the session_records row, 0 = no row
          inbox,      \* spool entries [h |-> hook, k |-> key, bad |-> BOOLEAN]
          hpc,        \* each hook's step
          hkey,       \* each hook's key (0 until it has one)
          ans,        \* the answer each hook got: "none" | "recorded" | "older" | "invalid" | "ns"
          jobs,       \* the core's work in flight: [src |-> "req"|"drain", e |-> entry, st |-> "txn"|"proj"|"wr", out, r]
          rc,         \* a blind reconcile/prune between its look and its write: [act, v], or [act |-> "none"]
          up,         \* the core (daemon or embedded) can answer
          clock,      \* next key
          oldKeys,    \* keys written by old hooks (no recordedAt in their file)
          made,       \* ghost: valid records a hook handed off (wrote the file, wrote a spool entry, or called)
          spooled,    \* ghost: every spool entry written
          consumed,   \* ghost: spool entries taken (into the row, into the file, or answered and deleted by their hook)
          pruneMark,  \* ghost: the newest key a prune removed
          crashes, prunes,
          backwards   \* ghost: the core once replaced the file with an older record than it held

vars == <<phase, file, row, inbox, hpc, hkey, ans, jobs, rc, up, clock, oldKeys, made, spooled, consumed,
          pruneMark, crashes, prunes, backwards>>

NoRc == [act |-> "none", v |-> 0, rs |-> 0]

\* --- modes ------------------------------------------------------------------------------------------------------
Tomb == phase \in {"tomb", "reread", "on", "rb"}
SwitchMore == phase \in {"on", "rb", "rbx"}                \* the switch file says switched or rolling-back
Writer == Tomb \/ SwitchMore                               \* kith_registry.switched: either source
Serving == phase = "on" /\ up

\* --- order ------------------------------------------------------------------------------------------------------
Ok(k) == IF k = 0 THEN -1 ELSE IF k \in oldKeys /\ MissingKey = "oldest" THEN 0 ELSE k
Newer(a, b) == Ok(a) > Ok(b)

\* The core writes the file (a rename): note whether it replaced a newer record.
CoreSetFile(v) == /\ file' = v
                  /\ backwards' = (backwards \/ (v # 0 /\ v < file))

LockHeld == (\E j \in jobs : j.st \in {"wr", "adopt"}) \/ rc.act # "none"

\* Spool writes and deletes by name: unique names, or one name per sid.
SpoolWrite(e) == inbox' = IF OneName THEN {e} ELSE inbox \cup {e}
SpoolDelete(e) == inbox' = IF OneName THEN {} ELSE inbox \ {e}

Valid(S) == {e \in S : ~e.bad}
Newest(S) == CHOOSE e \in S : \A f \in S : Ok(f.k) <= Ok(e.k)

\* The Files-mode drain (kith-registry drain, from a Files-mode hook or kith-sched): move the newest valid entry into
\* place if it is newer than the file, delete every entry (an invalid one is dropped as the core would).
FilesDrain == /\ inbox # {}
              /\ file' = IF Valid(inbox) # {} /\ Newer(Newest(Valid(inbox)).k, file) THEN Newest(Valid(inbox)).k ELSE file
              /\ inbox' = {}
              /\ consumed' = consumed \cup inbox

Init == /\ phase = "files0" /\ file = 0 /\ row = 0 /\ inbox = {}
        /\ hpc = [i \in Hooks |-> "wait"] /\ hkey = [i \in Hooks |-> 0] /\ ans = [i \in Hooks |-> "none"]
        /\ jobs = {} /\ rc = NoRc /\ up = TRUE /\ clock = 1 /\ oldKeys = {}
        /\ made = {} /\ spooled = {} /\ consumed = {} /\ pruneMark = 0 /\ crashes = 0 /\ prunes = 0
        /\ backwards = FALSE

Entry(i) == [h |-> i, k |-> hkey[i], bad |-> Kinds[i] = "bad"]
Made(i) == IF Kinds[i] = "bad" THEN made ELSE made \cup {hkey[i]}

\* =================================================================================================================
\* Hooks (kernel/bin/kith-sessions record -> kith_registry.record)
\* =================================================================================================================
\* A hook starts once the one before it has ended. A new hook takes its recordedAt and reads the mode once.
HookStart(i) ==
  /\ hpc[i] = "wait"
  /\ IF i = 1 THEN TRUE ELSE hpc[i - 1] = "done"
  /\ IF Kinds[i] = "old"
       THEN /\ hpc' = [hpc EXCEPT ![i] = "o_write"] /\ UNCHANGED <<hkey, clock>>
     ELSE IF Kinds[i] = "bad" /\ ~Writer
       THEN /\ hpc' = [hpc EXCEPT ![i] = "done"] /\ UNCHANGED <<hkey, clock>>   \* not modelled in Files mode
     ELSE /\ hkey' = [hkey EXCEPT ![i] = clock] /\ clock' = clock + 1
          /\ hpc' = [hpc EXCEPT ![i] = IF ~Writer THEN "f_write" ELSE IF WriteAhead THEN "w_spool" ELSE "w_call"]
  /\ UNCHANGED <<phase, file, row, inbox, ans, jobs, rc, up, oldKeys, made, spooled, consumed, pruneMark, crashes, prunes, backwards>>

\* An old-kernel hook: writes <sid>.json itself (no recordedAt: its order key is its mtime), ignores the marker.
OldWrite(i) ==
  /\ hpc[i] = "o_write"
  /\ hkey' = [hkey EXCEPT ![i] = clock] /\ clock' = clock + 1
  /\ file' = clock /\ oldKeys' = oldKeys \cup {clock} /\ made' = made \cup {clock}
  /\ hpc' = [hpc EXCEPT ![i] = "done"]
  /\ UNCHANGED <<phase, row, inbox, ans, jobs, rc, up, spooled, consumed, pruneMark, crashes, prunes, backwards>>

\* Files mode: write the file (as today, now with recordedAt) ...
FilesWrite(i) ==
  /\ hpc[i] = "f_write"
  /\ file' = hkey[i] /\ made' = Made(i)
  /\ hpc' = [hpc EXCEPT ![i] = "f_drain"]
  /\ UNCHANGED <<phase, row, inbox, hkey, ans, jobs, rc, up, clock, oldKeys, spooled, consumed, pruneMark, crashes, prunes, backwards>>

\* ... drain the inbox into place ...
FilesHookDrain(i) ==
  /\ hpc[i] = "f_drain"
  /\ IF inbox = {} THEN UNCHANGED <<file, inbox, consumed>> ELSE FilesDrain
  /\ hpc' = [hpc EXCEPT ![i] = "f_check"]
  /\ UNCHANGED <<phase, row, hkey, ans, jobs, rc, up, clock, oldKeys, made, spooled, pruneMark, crashes, prunes, backwards>>

\* ... and re-check the mode: if Writer mode turned on, also do the Writer-mode steps (claims takers' rule).
FilesRecheck(i) ==
  /\ hpc[i] = "f_check"
  /\ hpc' = [hpc EXCEPT ![i] = IF ~Writer THEN "done" ELSE IF WriteAhead THEN "w_spool" ELSE "w_call"]
  /\ UNCHANGED <<phase, file, row, inbox, hkey, ans, jobs, rc, up, clock, oldKeys, made, spooled, consumed, pruneMark, crashes, prunes, backwards>>

\* Writer mode step 1 (MUST 3): the spool entry first, under a name of its own.
Spool(i) ==
  /\ hpc[i] = "w_spool"
  /\ SpoolWrite(Entry(i)) /\ spooled' = spooled \cup {Entry(i)} /\ made' = Made(i)
  /\ hpc' = [hpc EXCEPT ![i] = "w_call"]
  /\ UNCHANGED <<phase, file, row, hkey, ans, jobs, rc, up, clock, oldKeys, consumed, pruneMark, crashes, prunes, backwards>>

\* Step 2: kith kernel core-call session.record. Not serving (stepping, rolling back, a marker with no switch, the
\* embedded core busy under the step's lifetime lock): refused not-serving or no answer, the same to the hook.
\* Switched but the core is down: no answer until the hook's timeout.
Call(i) ==
  /\ hpc[i] = "w_call"
  /\ made' = Made(i)     \* without write-ahead the record is handed off here
  /\ IF Serving
       THEN /\ jobs' = jobs \cup {[src |-> "req", e |-> Entry(i), st |-> "txn", out |-> "none", r |-> 0, f |-> 0, rs |-> 0]}
            /\ UNCHANGED ans
     ELSE IF phase = "on"
       THEN UNCHANGED <<jobs, ans>>
     ELSE /\ ans' = [ans EXCEPT ![i] = "ns"] /\ UNCHANGED jobs
  /\ hpc' = [hpc EXCEPT ![i] = "w_wait"]
  /\ UNCHANGED <<phase, file, row, inbox, hkey, rc, up, clock, oldKeys, spooled, consumed, pruneMark, crashes, prunes, backwards>>

\* Step 3: an answer. recorded/older/invalid: delete its own entry. not-serving: keep it (MUST 5).
Answered(i) ==
  /\ hpc[i] = "w_wait" /\ ans[i] # "none"
  /\ IF ans[i] = "ns" /\ ~NsDrops
       THEN /\ UNCHANGED <<inbox, consumed>>
            /\ hpc' = [hpc EXCEPT ![i] = IF WriteAhead THEN "done" ELSE "w_late"]
     ELSE /\ IF WriteAhead THEN SpoolDelete(Entry(i)) ELSE UNCHANGED inbox
          /\ consumed' = IF ans[i] = "ns" THEN consumed ELSE consumed \cup {Entry(i)}
          /\ hpc' = [hpc EXCEPT ![i] = "done"]
  /\ UNCHANGED <<phase, file, row, hkey, ans, jobs, rc, up, clock, oldKeys, made, spooled, pruneMark, crashes, prunes, backwards>>

\* The hook's own timeout (<= 3 s) with no answer: write-ahead keeps the entry; call-first spools now.
TimeOut(i) ==
  /\ hpc[i] = "w_wait" /\ ans[i] = "none"
  /\ hpc' = [hpc EXCEPT ![i] = IF WriteAhead THEN "done" ELSE "w_late"]
  /\ UNCHANGED <<phase, file, row, inbox, hkey, ans, jobs, rc, up, clock, oldKeys, made, spooled, consumed, pruneMark, crashes, prunes, backwards>>

\* Call first (the broken order): spool after a no-answer.
LateSpool(i) ==
  /\ hpc[i] = "w_late"
  /\ SpoolWrite(Entry(i)) /\ spooled' = spooled \cup {Entry(i)}
  /\ hpc' = [hpc EXCEPT ![i] = "done"]
  /\ UNCHANGED <<phase, file, row, hkey, ans, jobs, rc, up, clock, oldKeys, made, consumed, pruneMark, crashes, prunes, backwards>>

\* Claude kills the hook at any step (its 5 s limit). Its in-flight call goes on in the core; its answer is lost.
Kill(i) ==
  /\ hpc[i] \notin {"wait", "done"}
  /\ hpc' = [hpc EXCEPT ![i] = "done"]
  /\ UNCHANGED <<phase, file, row, inbox, hkey, ans, jobs, rc, up, clock, oldKeys, made, spooled, consumed, pruneMark, crashes, prunes, backwards>>

\* =================================================================================================================
\* The core (P1: transcripts/record.rs, the daemon's reconcile + drain + prune)
\* =================================================================================================================
\* session.record's transaction: validation, then newer-wins against the row, upsert, commit.
Txn(j) ==
  /\ j \in jobs /\ j.st = "txn"
  /\ IF j.e.bad
       THEN /\ jobs' = (jobs \ {j}) \cup {[j EXCEPT !.st = "done", !.out = "invalid"]} /\ UNCHANGED row
     ELSE IF Newer(j.e.k, row)
       THEN /\ row' = j.e.k /\ jobs' = (jobs \ {j}) \cup {[j EXCEPT !.st = "proj", !.out = "recorded"]}
     ELSE /\ UNCHANGED row /\ jobs' = (jobs \ {j}) \cup {[j EXCEPT !.st = "proj", !.out = "older"]}
  /\ UNCHANGED <<phase, file, inbox, hpc, hkey, ans, rc, up, clock, oldKeys, made, spooled, consumed, pruneMark, crashes, prunes, backwards>>

\* What the record path writes into the file (MUST 2: from the row read back, also when the record lost as older).
Target(j) == IF FileFrom = "request" THEN j.e.k ELSE row

\* After the commit, under the per-sid lock: the file write. With ProjectRule "reconcile" the settle first looks at the
\* file (and takes the lock); a newer file (an old hook's) is then adopted by ProjectAdopt, in a step of its own, as
\* the code does (project.rs settle_with, A7). With "row" the row is written whatever the file holds.
Project(j) ==
  /\ j \in jobs /\ j.st = "proj" /\ ~LockHeld
  /\ IF ProjectRule = "reconcile"
       THEN /\ jobs' = (jobs \ {j}) \cup {[j EXCEPT !.st = "adopt", !.f = file, !.rs = row]}
            /\ UNCHANGED <<file, row, backwards>>
     ELSE IF FileWrite = "atomic"
       THEN /\ CoreSetFile(Target(j)) /\ UNCHANGED row
            /\ jobs' = (jobs \ {j}) \cup {[j EXCEPT !.st = "done"]}
     ELSE /\ UNCHANGED <<file, row, backwards>>
          /\ jobs' = (jobs \ {j}) \cup {[j EXCEPT !.st = "wr", !.r = Target(j)]}
  /\ UNCHANGED <<phase, inbox, hpc, hkey, ans, rc, up, clock, oldKeys, made, spooled, consumed, pruneMark, crashes, prunes>>

\* The adoption and the file write, after the look. The row is written by the record's own newer-wins upsert (a commit
\* that landed since the look is kept) or, in the broken variant, from what the look read. Then the file is written from
\* the row as it is now, unless the file is newer than that (swapped out and checked, FileWrite "atomic").
Adopted(f, rs) == IF RowWrite = "conditional" THEN Newer(f, row) ELSE Newer(f, rs)
ProjectAdopt(j) ==
  /\ j \in jobs /\ j.st = "adopt"
  /\ IF Adopted(j.f, j.rs)
       THEN /\ row' = j.f /\ UNCHANGED <<file, backwards>>
            /\ jobs' = (jobs \ {j}) \cup {[j EXCEPT !.st = "done"]}
     ELSE /\ UNCHANGED row
          /\ IF FileWrite = "atomic"
               THEN /\ IF FileFrom = "row" /\ Newer(file, row) THEN UNCHANGED <<file, backwards>> ELSE CoreSetFile(Target(j))
                    /\ jobs' = (jobs \ {j}) \cup {[j EXCEPT !.st = "done"]}
               ELSE /\ UNCHANGED <<file, backwards>>
                    /\ jobs' = (jobs \ {j}) \cup {[j EXCEPT !.st = "wr", !.r = Target(j)]}
  /\ UNCHANGED <<phase, inbox, hpc, hkey, ans, rc, up, clock, oldKeys, made, spooled, consumed, pruneMark, crashes, prunes>>

\* Blind: the rename of what was looked at, later.
ProjectWrite(j) ==
  /\ j \in jobs /\ j.st = "wr"
  /\ CoreSetFile(j.r)
  /\ jobs' = (jobs \ {j}) \cup {[j EXCEPT !.st = "done"]}
  /\ UNCHANGED <<phase, row, inbox, hpc, hkey, ans, rc, up, clock, oldKeys, made, spooled, consumed, pruneMark, crashes, prunes>>

\* The answer, only after the rename. A drained entry is deleted once it is in.
Finish(j) ==
  /\ j \in jobs /\ j.st = "done"
  /\ jobs' = jobs \ {j}
  /\ IF j.src = "req"
       THEN /\ ans' = IF hpc[j.e.h] = "w_wait" THEN [ans EXCEPT ![j.e.h] = j.out] ELSE ans
            /\ UNCHANGED <<inbox, consumed>>
     ELSE /\ SpoolDelete(j.e) /\ consumed' = consumed \cup {j.e} /\ UNCHANGED ans
  /\ UNCHANGED <<phase, file, row, hpc, hkey, rc, up, clock, oldKeys, made, spooled, pruneMark, crashes, prunes, backwards>>

\* The daemon's drain (at start and every 60 s, only while switched): one entry at a time through the record path,
\* racing live records.
DrainStart(e) ==
  /\ Serving /\ e \in inbox
  /\ ~\E j \in jobs : j.src = "drain"
  /\ jobs' = jobs \cup {[src |-> "drain", e |-> e, st |-> "txn", out |-> "none", r |-> 0, f |-> 0, rs |-> 0]}
  /\ UNCHANGED <<phase, file, row, inbox, hpc, hkey, ans, rc, up, clock, oldKeys, made, spooled, consumed, pruneMark, crashes, prunes, backwards>>

\* The daemon's reconcile (MUST 1), under the per-sid lock: a file newer than its row is adopted (a look here, the
\* conditional write in RcFinish, A7); a row whose file differs or is missing is written to the file.
Reconcile1 ==
  /\ Reconcile /\ Serving /\ ~LockHeld /\ file # row
  /\ IF Newer(file, row)
       THEN /\ rc' = [act |-> "adopt", v |-> file, rs |-> row] /\ UNCHANGED <<file, row, backwards>>
     ELSE IF FileWrite = "atomic"
       THEN /\ CoreSetFile(row) /\ UNCHANGED <<row, rc>>
     ELSE /\ rc' = [act |-> "write", v |-> row, rs |-> row] /\ UNCHANGED <<file, row, backwards>>
  /\ UNCHANGED <<phase, inbox, hpc, hkey, ans, jobs, up, clock, oldKeys, made, spooled, consumed, pruneMark, crashes, prunes>>

\* The hourly prune in the core (kith-sched's rule: old and its pid gone), under the per-sid lock: judges the row now
\* (a look); RcFinish deletes the file and the row, the row only where its key is still the judged one. Not while the
\* file is newer than the row (that one is reconciled first).
CorePrune ==
  /\ Serving /\ ~LockHeld /\ prunes < MaxPrune /\ row # 0
  /\ prunes' = prunes + 1
  /\ (FileWrite = "atomic" => ~Newer(file, row))
  /\ rc' = [act |-> "prune", v |-> row, rs |-> row] /\ UNCHANGED <<file, row, pruneMark>>
  /\ UNCHANGED <<phase, inbox, hpc, hkey, ans, jobs, up, clock, oldKeys, made, spooled, consumed, crashes, backwards>>

\* The write or unlink after the look.
RcFinish ==
  /\ rc.act # "none"
  /\ IF rc.act = "write"
       THEN /\ CoreSetFile(rc.v) /\ UNCHANGED <<row, pruneMark>>
     ELSE IF rc.act = "adopt"
       THEN /\ UNCHANGED <<pruneMark>>
            /\ IF Adopted(rc.v, rc.rs)
                 THEN /\ row' = rc.v /\ UNCHANGED <<file, backwards>>
               ELSE /\ UNCHANGED row
                    /\ IF Newer(file, row) \/ file = row THEN UNCHANGED <<file, backwards>> ELSE CoreSetFile(row)
     ELSE IF FileWrite = "atomic" /\ Newer(file, rc.v)
       THEN UNCHANGED <<file, row, pruneMark, backwards>>
     ELSE /\ file' = 0
          /\ row' = (IF RowWrite = "conditional" /\ row # rc.v THEN row ELSE 0)
          /\ pruneMark' = Max2(pruneMark, rc.v)
          /\ UNCHANGED backwards
  /\ rc' = NoRc
  /\ UNCHANGED <<phase, inbox, hpc, hkey, ans, jobs, up, clock, oldKeys, made, spooled, consumed, crashes, prunes>>

\* A crash anywhere: between commit and rename, or between a blind look and its write.
Crash ==
  /\ up /\ crashes < MaxCrash
  /\ up' = FALSE /\ jobs' = {} /\ rc' = NoRc /\ crashes' = crashes + 1
  /\ UNCHANGED <<phase, file, row, inbox, hpc, hkey, ans, clock, oldKeys, made, spooled, consumed, pruneMark, prunes, backwards>>

Restart ==
  /\ ~up /\ up' = TRUE
  /\ UNCHANGED <<phase, file, row, inbox, hpc, hkey, ans, jobs, rc, clock, oldKeys, made, spooled, consumed, pruneMark, crashes, prunes, backwards>>

\* =================================================================================================================
\* kith-sched in Files mode (prune_terminal, and the inbox drain)
\* =================================================================================================================
SchedDrain ==
  /\ ~Writer /\ FilesDrain
  /\ UNCHANGED <<phase, row, hpc, hkey, ans, jobs, rc, up, clock, oldKeys, made, spooled, pruneMark, crashes, prunes, backwards>>

SchedPrune ==
  /\ ~Writer /\ prunes < MaxPrune /\ file # 0
  /\ file' = 0 /\ pruneMark' = Max2(pruneMark, file) /\ prunes' = prunes + 1
  /\ UNCHANGED <<phase, row, inbox, hpc, hkey, ans, jobs, rc, up, clock, oldKeys, made, spooled, consumed, crashes, backwards>>

\* =================================================================================================================
\* The step, cancel, rollback (P3: cutover/modules/transcripts.rs)
\* =================================================================================================================
StepVars == <<hpc, hkey, ans, clock, oldKeys, made, spooled, pruneMark, crashes, prunes>>

\* Preflight passed (nothing left in the inbox: preflight (7)); the switch file says stepping.
Begin ==
  /\ phase = "files0" /\ inbox = {}
  /\ phase' = "pre"
  /\ UNCHANGED <<file, row, inbox, jobs, rc, up, consumed, backwards>> /\ UNCHANGED StepVars

Import ==
  /\ phase = "pre" /\ phase' = "imp" /\ row' = file
  /\ UNCHANGED <<file, inbox, jobs, rc, up, consumed, backwards>> /\ UNCHANGED StepVars

Tombstone ==
  /\ phase = "imp" /\ phase' = "tomb"
  /\ UNCHANGED <<file, row, inbox, jobs, rc, up, consumed, backwards>> /\ UNCHANGED StepVars

\* before_tombstone's re-read under the lifetime lock: upsert what differs.
Reread ==
  /\ phase = "tomb" /\ phase' = "reread" /\ row' = IF file # 0 THEN file ELSE row
  /\ UNCHANGED <<file, inbox, jobs, rc, up, consumed, backwards>> /\ UNCHANGED StepVars

\* The framework's marker, then the switch record: the core serves. The daemon starts (reconcile + drain).
Switch ==
  /\ phase = "reread" /\ phase' = "on" /\ jobs' = {} /\ rc' = NoRc /\ up' = TRUE
  /\ UNCHANGED <<file, row, inbox, consumed, backwards>> /\ UNCHANGED StepVars

\* cancel_step: remove the marker. Files were written all along; a spool left by a not-serving answer is drained by
\* the Files-mode drains.
Cancel ==
  /\ phase \in {"pre", "imp", "tomb", "reread"} /\ phase' = "files"
  /\ UNCHANGED <<file, row, inbox, jobs, rc, up, consumed, backwards>> /\ UNCHANGED StepVars

\* Rollback: the record says rolling-back (hooks stay in Writer mode and spool); the daemon is stopped.
Rollback ==
  /\ phase = "on" /\ phase' = "rb" /\ jobs' = {} /\ rc' = NoRc
  /\ UNCHANGED <<file, row, inbox, up, consumed, backwards>> /\ UNCHANGED StepVars

\* export_back: drain the inbox into the row (newer-wins), then write the row back to its file.
ExportBack ==
  /\ phase = "rb" /\ phase' = "rbx"
  /\ LET r1 == IF Valid(inbox) # {} /\ Newer(Newest(Valid(inbox)).k, row) THEN Newest(Valid(inbox)).k ELSE row
         f1 == IF r1 = 0 THEN file
               ELSE IF ExportRule = "different" THEN r1
               ELSE IF Newer(r1, file) THEN r1 ELSE file
     IN /\ row' = r1 /\ CoreSetFile(f1)
  /\ inbox' = {} /\ consumed' = consumed \cup inbox
  /\ UNCHANGED <<jobs, rc, up>> /\ UNCHANGED StepVars

\* The framework removes the marker and the switch record: Files mode again.
RollbackDone ==
  /\ phase = "rbx" /\ phase' = "files"
  /\ UNCHANGED <<file, row, inbox, jobs, rc, up, consumed, backwards>> /\ UNCHANGED StepVars

\* =================================================================================================================
HookStep(i) == FilesWrite(i) \/ FilesHookDrain(i) \/ FilesRecheck(i) \/ Spool(i) \/ Call(i) \/ Answered(i)
               \/ TimeOut(i) \/ LateSpool(i) \/ OldWrite(i)
CoreStep(j) == Txn(j) \/ Project(j) \/ ProjectAdopt(j) \/ ProjectWrite(j) \/ Finish(j)
StepProgress == Import \/ Tombstone \/ Reread \/ Switch \/ ExportBack \/ RollbackDone

Next == \/ \E i \in Hooks : HookStart(i) \/ HookStep(i) \/ Kill(i)
        \/ \E j \in jobs : CoreStep(j)
        \/ \E e \in inbox : DrainStart(e)
        \/ Reconcile1 \/ CorePrune \/ RcFinish \/ Crash \/ Restart
        \/ SchedDrain \/ SchedPrune
        \/ Begin \/ StepProgress \/ Cancel \/ Rollback

\* Fair: every hook's own steps (not its start, not the kill), the core's work, its restart, the 60 s reconcile and
\* drain, kith-sched's hourly pass, and a step once begun (not its start, cancel or rollback; not crashes or prunes).
Fairness == /\ \A i \in Hooks : WF_vars(HookStep(i))
            /\ WF_vars(\E j \in jobs : CoreStep(j))
            /\ WF_vars(\E e \in inbox : DrainStart(e))
            /\ WF_vars(Reconcile1) /\ WF_vars(RcFinish) /\ WF_vars(Restart)
            /\ WF_vars(SchedDrain)
            /\ WF_vars(StepProgress)

Spec == Init /\ [][Next]_vars /\ Fairness

\* =================================================================================================================
\* Properties
\* =================================================================================================================
TypeOK == /\ file \in Keys /\ row \in Keys /\ clock \in 1..(MaxKey + 1)
          /\ phase \in {"files0", "pre", "imp", "tomb", "reread", "on", "rb", "rbx", "files"}
          /\ \A e \in inbox : e.k \in Keys

\* The newest valid record a hook handed off that no prune removed after it.
Live == {k \in made : k > pruneMark}
\* Where the readers read in their mode holds it: the file (Phase A readers); once switched also the row.
InPlace == Live # {} => /\ file = Max(Live)
                        /\ (phase = "on" => row = Max(Live))

\* Every record a hook made ends up, once things settle, as the newest in the place the readers read, and the inbox
\* empties.
Converges == <>[](InPlace /\ inbox = {})

\* No file ever goes backwards: the core never replaces the file with an older record than the one it holds.
NeverBackwards == ~backwards

\* Spool entries never erase each other: an entry nobody has taken is still in the inbox.
SpoolKept == spooled \ consumed \subseteq inbox

\* An invalid record is never listed.
BadNeverListed == \A i \in Hooks : Kinds[i] = "bad" /\ hkey[i] # 0 => file # hkey[i] /\ row # hkey[i]

\* --- non-vacuity: each of these happens (the invariant FAILS) ----------------------------------------------------
NeverSwitched == phase # "on"
NeverRolledBack == phase # "rbx"
NeverOldWhileOn == ~(phase = "on" /\ Newer(file, row) /\ file \in oldKeys)
NeverStale == ~(phase = "on" /\ Newer(row, file) /\ jobs = {} /\ rc.act = "none")   \* crash between commit and rename
NeverDrainedByCore == ~\E j \in jobs : j.src = "drain"
NeverNotServing == \A i \in Hooks : ans[i] # "ns"
NeverPruned == prunes = 0
NeverInvalid == \A i \in Hooks : ans[i] # "invalid"
\* The look of a settle, a reconcile or a prune is overtaken by a commit before its write (A7).
NeverOvertaken == ~((\E j \in jobs : j.st = "adopt" /\ row # j.rs) \/ (rc.act \in {"adopt", "prune"} /\ row # rc.rs))
NeverRacing == ~\E j1, j2 \in jobs : j1.src = "req" /\ j2.src = "drain"
====
