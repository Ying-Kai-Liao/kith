---- MODULE RoomsRename ----
\* Rooms with people on a switched Mac: a person renamed in the core while the host keeps its rows under the old id.
\* One person, one room. Ids 1..N; the core's current id is `cur`, every id <= cur is a former or current id of the
\* person (held_under returns 1..cur; a former id is never handed out again).
\* Actors, in any order: the core's Rename; Kai's add / remove / invite (serial, from the app); the invite's prove.
\* Each host op reads the person's ids (an async core call, held_under) BEFORE it takes the membership lock, then
\* commits in one transaction under it. So a Rename can land between the read and the commit.
\* The architect's model (designs/2026-11-16-rooms-rename), copied as written. The code (#1013) is Resolve = "all",
\* Key = "root" (`Held::first`), Reread = FALSE (a removal does not read the ids again after its commit): rows
\* rename-code and rename-code-paired in checks.tsv. Code: remote/core_revokes.rs `held`, chat/room_people.rs
\* (add_person_for, remove_person, live_seat_held), store/room_people.rs (join_room_person_for, leave_room_held).
EXTENDS Naturals, FiniteSets

CONSTANTS N,
          Resolve,   \* "given": the op uses only the id it was handed (today's code); "all": every id held_under returns
          Key,       \* where a join writes the stay: "given" (the id handed in), "held" (the id the host holds the
                     \* person's devices under, id 1, when Paired, else the current id read)
          Paired,    \* the person has a device on this host (rows under id 1)
          Reread     \* a remove re-reads the ids after its commit and closes again if the set grew

Ids == 1..N

VARIABLES cur, stay, inv, partDone, kai, kSnap, kKey, want, pv, pSnap, pKey

vars == <<cur, stay, inv, partDone, kai, kSnap, kKey, want, pv, pSnap, pKey>>

Snap(given) == IF Resolve = "all" THEN 1..cur ELSE {given}
\* "root": the person's first id (was[0], or the id when never renamed); "held": the devices' id when Paired, else
\* the current id read (fails OneStay: two joins that read either side of a rename write under two ids).
KeyOf(given) == CASE Key = "given" -> given
                  [] Key = "root"  -> 1
                  [] Key = "held"  -> IF Paired THEN 1 ELSE cur

Init == /\ cur = 1 /\ stay = [i \in Ids |-> FALSE] /\ inv = 0 /\ partDone = FALSE
        /\ kai = "idle" /\ kSnap = {} /\ kKey = 0 /\ want = "none"
        /\ pv = "idle" /\ pSnap = {} /\ pKey = 0

Rename == cur < N /\ cur' = cur + 1 /\ UNCHANGED <<stay, inv, partDone, kai, kSnap, kKey, want, pv, pSnap, pKey>>

\* Kai adds the person by the id the app shows (the current one).
AddRead == kai = "idle" /\ kai' = "add" /\ kSnap' = Snap(cur) /\ kKey' = KeyOf(cur)
           /\ UNCHANGED <<cur, stay, inv, partDone, want, pv, pSnap, pKey>>
AddCommit == kai = "add" /\ kai' = "idle" /\ want' = "in"
             /\ stay' = (IF \E i \in kSnap : stay[i] THEN stay ELSE [stay EXCEPT ![kKey] = TRUE])
             /\ UNCHANGED <<cur, inv, partDone, kSnap, kKey, pv, pSnap, pKey>>

\* Kai removes the person: closes every open stay under the ids read, and marks done the waiting room part of an invite
\* held under one of them.
RmRead == kai = "idle" /\ kai' = "rm" /\ kSnap' = Snap(cur)
          /\ UNCHANGED <<cur, stay, inv, partDone, kKey, want, pv, pSnap, pKey>>
RmCommit == kai = "rm" /\ kai' = "rmcheck"
            /\ stay' = [i \in Ids |-> IF i \in kSnap THEN FALSE ELSE stay[i]]
            /\ partDone' = (partDone \/ inv \in kSnap)
            /\ UNCHANGED <<cur, inv, kSnap, kKey, want, pv, pSnap, pKey>>
RmCheck == kai = "rmcheck"
           /\ (IF Reread /\ Resolve = "all" /\ 1..cur # kSnap
                 THEN kai' = "rm" /\ kSnap' = 1..cur /\ UNCHANGED want
                 ELSE kai' = "idle" /\ want' = "out" /\ UNCHANGED kSnap)
           /\ UNCHANGED <<cur, stay, inv, partDone, kKey, pv, pSnap, pKey>>

\* Kai invites the person into the room (once): the invite row is under the current id; its room part waits.
Invite == kai = "idle" /\ inv = 0 /\ inv' = cur /\ partDone' = FALSE /\ want' = "in"
          /\ UNCHANGED <<cur, stay, kai, kSnap, kKey, pv, pSnap, pKey>>

\* The person proves the invite: the join and the part's done mark in one transaction; a part already done joins no one.
ProveRead == pv = "idle" /\ inv # 0 /\ pv' = "read" /\ pSnap' = Snap(inv) /\ pKey' = KeyOf(inv)
             /\ UNCHANGED <<cur, stay, inv, partDone, kai, kSnap, kKey, want>>
ProveCommit == pv = "read" /\ pv' = "done"
               /\ (IF partDone THEN UNCHANGED <<stay, partDone>>
                   ELSE /\ partDone' = TRUE
                        /\ stay' = (IF \E i \in pSnap : stay[i] THEN stay ELSE [stay EXCEPT ![pKey] = TRUE]))
               /\ UNCHANGED <<cur, inv, kai, kSnap, kKey, want, pSnap, pKey>>

Next == Rename \/ AddRead \/ AddCommit \/ RmRead \/ RmCommit \/ RmCheck \/ Invite \/ ProveRead \/ ProveCommit

Spec == Init /\ [][Next]_vars

TypeOK == cur \in Ids /\ stay \in [Ids -> BOOLEAN] /\ kai \in {"idle", "add", "rm", "rmcheck"}

Open == {i \in Ids : stay[i]}

\* Kai's removal sticks: once a remove has returned and Kai has not added or invited since, the person is in no stay.
RemoveSticks == (kai = "idle" /\ want = "out") => Open = {}

\* One person, one open stay (under whichever id).
OneStay == Cardinality(Open) <= 1

\* The person's own devices (under id 1, when Paired) find the stay they were let in by: liveness of reads, fails closed.
ReaderFinds == (Paired /\ Open # {}) => stay[1]
====
