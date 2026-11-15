---- MODULE RoomJobs ----
\* Rooms with people, Q2 full (Kai 2026-11-13), access side. Re-keys the Oct RoomDoor model (J2-J5) to Kai's
\* Q1 pick (finish-hidden): a join stops nothing and does not narrow a running turn (its output is hidden from
\* the joiner by the gen stamp, Rooms3). A turn's grant is fixed at its start as the meet over the people in the
\* room then (its basis); it narrows live only when a basis person is narrowed (untick, revoke); a door start is
\* one locked step that checks the basis people's live grants (D2); an untick or revoke ends the doors, and a
\* revoke the turn, that leaned on that person (D1/J4). Ranks: 0 none, 1 the job, 2 owner level (no people).
\* Room-person processes are never reused warm (as every guest run today), so the sandbox and env are taken at
\* spawn = turn start. Checks the design, not code.
\*
\* The switched modes (authority in the core, #975): an untick or revoke commits in the core WITHOUT the door lock and
\* leaves the person's sweep owed (`owed`); a start takes the lock, checks, then writes the claim and starts (DoorCheck,
\* DoorGo), so a commit can land between; the sweep (kith door sweep, from the host's cut_off, 60 s backstop) takes the lock,
\* ends every claim whose basis holds the person and clears the debt. Kai's pick: accept this, no door-table or lock fix.
EXTENDS Naturals, FiniteSets

CONSTANTS People, MaxTurns, Doors,
          Mode  \* "fixed" | "liveonly" | "snaponly" | "split" | "sweepperson" | "revokekeepsturn" | "joinnarrows"
              \* | "switched" | "switchednolock" | "switchedsweepperson"

VARIABLES inroom, ticked, may, turn, turns, door, widened, stale, owed, lock
vars == <<inroom, ticked, may, turn, turns, door, widened, stale, owed, lock>>

Switched == Mode \in {"switched", "switchednolock", "switchedsweepperson"}

Members == {p \in People : inroom[p]}
RankOf(S) == IF S = {} THEN 2 ELSE IF \A p \in S : ticked[p] /\ may[p] THEN 1 ELSE 0
Min(a, b) == IF a < b THEN a ELSE b
\* What a door start in the running turn is checked against.
Eff == CASE Mode = "liveonly"    -> RankOf(Members)                       \* the room now, no turn cap
         [] Mode = "snaponly"    -> turn.rank                             \* start snapshot only
         [] Mode = "joinnarrows" -> Min(turn.rank, RankOf(Members))       \* Oct J2: a join narrows too
         [] OTHER                -> IF turn.basis = {} THEN turn.rank     \* owner level: Kai's own access
                                    ELSE Min(turn.rank, RankOf(turn.basis))

NoTurn == [st |-> "idle", rank |-> 0, basis |-> {}]

Init ==
  /\ inroom = [p \in People |-> FALSE] /\ ticked = [p \in People |-> FALSE] /\ may = [p \in People |-> TRUE]
  /\ turn = NoTurn /\ turns = 0
  /\ door = [k \in Doors |-> [st |-> "none", rank |-> 0, basis |-> {}]]
  /\ widened = FALSE /\ stale = FALSE /\ owed = {} /\ lock = FALSE

\* Q1 finish-hidden: a join stops nothing.
Add(p) ==
  /\ ~inroom[p] /\ may[p] /\ inroom' = [inroom EXCEPT ![p] = TRUE]
  /\ UNCHANGED <<ticked, may, turn, turns, door, widened, stale, owed, lock>>

Leave(p) ==
  /\ inroom[p] /\ inroom' = [inroom EXCEPT ![p] = FALSE]
  /\ UNCHANGED <<ticked, may, turn, turns, door, widened, stale, owed, lock>>

Tick(p) ==
  /\ ~ticked[p] /\ ticked' = [ticked EXCEPT ![p] = TRUE]
  /\ UNCHANGED <<inroom, may, turn, turns, door, widened, stale, owed, lock>>

Leaned(k, p) == door[k].st \in {"run", "pend"} /\ p \in door[k].basis

\* Kai unticks p's job; the sweep runs in the same locked step and ends every door that leaned on p.
Untick(p) ==
  /\ ticked[p] /\ ticked' = [ticked EXCEPT ![p] = FALSE]
  /\ door' = IF Mode = "sweepperson" \/ Switched THEN door
             ELSE [k \in Doors |-> IF Leaned(k, p) THEN [door[k] EXCEPT !.st = "stopped"] ELSE door[k]]
  /\ owed' = IF Switched THEN owed \cup {p} ELSE owed
  /\ UNCHANGED <<inroom, may, turn, turns, widened, stale, lock>>

\* The start's check and claim are one critical section of the turn's own process: a turn that ends (or is ended by a revoke)
\* leaves no half-done start and no lock held.
Settle == [k \in Doors |-> IF door[k].st = "pend" THEN [door[k] EXCEPT !.st = "stopped"] ELSE door[k]]
Ends(p) == Mode # "revokekeepsturn" /\ turn.st = "run" /\ p \in turn.basis

\* Kai revokes p: the same sweep, and the turn that leaned on p ends (as a guest's running turn does today).
Revoke(p) ==
  /\ may[p] /\ may' = [may EXCEPT ![p] = FALSE]
  /\ door' = IF Switched THEN (IF Ends(p) THEN Settle ELSE door)
             ELSE IF Mode = "sweepperson" THEN door
             ELSE [k \in Doors |-> IF Leaned(k, p) THEN [door[k] EXCEPT !.st = "stopped"] ELSE door[k]]
  /\ turn' = IF Ends(p) THEN NoTurn ELSE turn
  /\ lock' = IF Switched /\ Ends(p) THEN FALSE ELSE lock
  /\ owed' = IF Switched THEN owed \cup {p} ELSE owed
  /\ UNCHANGED <<inroom, ticked, turns, widened, stale>>

\* The switched sweep: waits for the lock (so a start between its check and its claim finishes first and its claim is
\* seen), ends every claim whose basis holds p, and clears what is owed. "switchednolock": it does not wait (a claim that
\* lands after it is missed); "switchedsweepperson": it matches the claim's person (the room), never the basis.
Sweep(p) ==
  /\ Switched /\ p \in owed /\ (Mode = "switchednolock" \/ ~lock)
  /\ door' = IF Mode = "switchedsweepperson" THEN door
             ELSE [k \in Doors |-> IF door[k].st = "run" /\ p \in door[k].basis THEN [door[k] EXCEPT !.st = "stopped"] ELSE door[k]]
  /\ owed' = owed \ {p}
  /\ UNCHANGED <<inroom, ticked, may, turn, turns, widened, stale, lock>>

Undo(p) ==
  /\ ~may[p] /\ may' = [may EXCEPT ![p] = TRUE]
  /\ UNCHANGED <<inroom, ticked, turn, turns, door, widened, stale, owed, lock>>

\* The turn's grant: the meet over the people in the room at its start, fixed for the turn.
StartTurn ==
  /\ turn.st = "idle" /\ turns < MaxTurns
  /\ \A p \in Members : may[p]
  /\ turn' = [st |-> "run", rank |-> RankOf(Members), basis |-> Members] /\ turns' = turns + 1
  /\ UNCHANGED <<inroom, ticked, may, door, widened, stale, owed, lock>>

EndTurn ==
  /\ turn.st = "run" /\ turn' = NoTurn
  /\ door' = IF Switched THEN Settle ELSE door
  /\ lock' = IF Switched THEN FALSE ELSE lock
  /\ UNCHANGED <<inroom, ticked, may, turns, widened, stale, owed>>

Started(need) == [st |-> "run", rank |-> need, basis |-> turn.basis]
Note(need) ==
  /\ widened' = (widened \/ need > turn.rank)
  /\ stale'   = (stale \/ (turn.basis # {} /\ need > RankOf(turn.basis)))

\* One locked step: check and start together.
DoorStart(k, need) ==
  /\ Mode # "split" /\ ~Switched
  /\ turn.st = "run" /\ door[k].st = "none" /\ Eff >= need
  /\ door' = [door EXCEPT ![k] = Started(need)]
  /\ Note(need)
  /\ UNCHANGED <<inroom, ticked, may, turn, turns, owed, lock>>

\* Broken: check, then start in a later step with no re-check.
DoorCheck(k, need) ==
  /\ Mode = "split" \/ (Switched /\ ~lock)
  /\ turn.st = "run" /\ door[k].st = "none" /\ Eff >= need
  /\ door' = [door EXCEPT ![k] = [st |-> "pend", rank |-> need, basis |-> turn.basis]]
  /\ lock' = Switched
  /\ UNCHANGED <<inroom, ticked, may, turn, turns, widened, stale, owed>>
DoorGo(k) ==
  /\ (Mode = "split" \/ Switched) /\ door[k].st = "pend" /\ turn.st = "run"
  /\ door' = [door EXCEPT ![k].st = "run"]
  /\ lock' = FALSE
  /\ Note(door[k].rank)
  /\ UNCHANGED <<inroom, ticked, may, turn, turns, owed>>

DoorEnd(k) ==
  /\ door[k].st = "run" /\ door' = [door EXCEPT ![k].st = "done"]
  /\ UNCHANGED <<inroom, ticked, may, turn, turns, widened, stale, owed, lock>>

Next ==
  \/ \E p \in People : Add(p) \/ Leave(p) \/ Tick(p) \/ Untick(p) \/ Revoke(p) \/ Undo(p) \/ Sweep(p)
  \/ StartTurn \/ EndTurn
  \/ \E k \in Doors, need \in {1, 2} : DoorStart(k, need) \/ DoorCheck(k, need)
  \/ \E k \in Doors : DoorGo(k) \/ DoorEnd(k)

Spec == Init /\ [][Next]_vars

\* R5 at start holds by construction (StartTurn takes RankOf(Members)). Not a state invariant: after an untick
\* the running turn's own sandbox (the job's folders and domains, taken at spawn) stays until the turn ends, the
\* same as a 1:1 guest run today (kith-auth job --rm only drops the tick); only doors are re-checked live.
\* R5 mid-turn: nothing starts above what the turn had at its start (a leave never widens a running turn).
NoWidenMidTurn == ~widened
\* J2 re-keyed: nothing starts above what the turn's people allow now (an untick or revoke is seen at the next start).
NarrowedForBasis == ~stale
\* J4: no running door leans on a person whose job was unticked or who was revoked.
\* Switched: the person's sweep is still owed (it runs under the lock, seconds after the commit).
SweepReachesRoomDoors == \A k \in Doors : door[k].st = "run" =>
                           \A p \in door[k].basis : (ticked[p] /\ may[p]) \/ p \in owed
\* Revoke ends the room turn that leaned on that person.
RevokeEndsTurn == turn.st = "run" => \A p \in turn.basis : may[p]
====
