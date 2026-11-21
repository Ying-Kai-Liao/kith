------------------------------ MODULE Rooms3 ------------------------------
(***************************************************************************)
(* Rooms2 + doors (Kai 2026-11-13: Q1 finish-hidden, Q2 full).             *)
(* A turn may start a door (a job's tool run as its own process, e.g. the  *)
(* render door). The door outlives its turn: its result is posted to the   *)
(* room later, after the turn ended, after a join, or after a host restart.*)
(* Q1 finish-hidden: a join stops nothing (the Oct J5 kill is dropped);    *)
(* the door's post carries its turn's start gen and level (rule 1), so it  *)
(* stays hidden from anyone who joined after.                              *)
(* Everything else is Rooms2 unchanged. Access ranks, door start checks    *)
(* and the revoke sweep are in RoomJobs.tla.                               *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS People, MaxEntries, MaxGen, MaxTurns, MaxRestarts, MaxQueued, MaxFlips,
          Track, FixInput, FreshSession, NoTag, Steer, ReuseIgnoresGen, KeepWarmOnEdit,
          NoGenQueue, RevokeDeletes, SplitPost, Hold,
          MaxDoors,        \* doors started in turns
          DoorStampLand    \* variant: a door result is stamped with the gen when it lands

Kai    == "kai"
Agent  == "planner"
Levels == {"owner", "room"}
Inf    == MaxGen + 1

VARIABLES gen, open, joined, ival, may, grant, entries, flips, turn, warm, sess,
          answered, started, restarts, pend, qnext, intent, snap, viol,
          doorOut,   \* door results not yet posted
          doors      \* doors started (bound)

vars == <<gen, open, joined, ival, may, grant, entries, flips, turn, warm, sess,
          answered, started, restarts, pend, qnext, intent, snap, viol, doorOut, doors>>

NoProc    == [on |-> FALSE, gen |-> 0, level |-> "none", people |-> {}, sgen |-> 0,
              lo |-> Inf, guest |-> FALSE, own |-> FALSE]
NoTurn    == [on |-> FALSE, gen |-> 0, level |-> "none", input |-> {}, startLen |-> 0,
              proc |-> NoProc]
EmptySess == [gen |-> 0, lo |-> Inf, guest |-> FALSE, own |-> FALSE]

Members   == {p \in People : open[p]}
MinOf(S)  == IF S = {} THEN Inf ELSE CHOOSE x \in S : \A y \in S : x <= y
MaxOf(S)  == IF S = {} THEN 0   ELSE CHOOSE x \in S : \A y \in S : x >= y
MaxJoined == MaxOf({joined[p] : p \in Members})
Room      == 1..Len(entries)
HasRoom   == Len(entries) < MaxEntries

Entry(a, g, l, lo, gu, ow) ==
  [author |-> a, gen |-> g, level |-> l, lo |-> lo, guest |-> gu, own |-> ow]
PostEntry(a, g) == Entry(a, g, "post", g, a \in People, FALSE)
Out(t) == Entry(Agent, t.gen, t.proc.level, t.proc.lo, t.proc.guest,
                t.proc.own \/ t.proc.level = "owner")

Absorb(pr, S) ==
  [pr EXCEPT !.lo    = MinOf({pr.lo} \cup {entries[i].lo : i \in S}),
             !.guest = pr.guest \/ (\E i \in S : entries[i].guest),
             !.own   = pr.own   \/ (\E i \in S : entries[i].own)]
SessOf(pr) == [gen |-> pr.sgen, lo |-> pr.lo, guest |-> pr.guest, own |-> pr.own]

Visible(p, i) == /\ open[p] /\ may[p]
                 /\ (NoTag \/ joined[p] <= entries[i].gen)

Tag(S) == viol \cup (S \cap Track)

Disarm == [q \in People |-> [snap[q] EXCEPT !.armed = FALSE]]
EndWarm == IF KeepWarmOnEdit THEN warm ELSE NoProc

Init ==
  /\ gen = 0
  /\ open = [p \in People |-> FALSE]
  /\ joined = [p \in People |-> 0]
  /\ ival = [p \in People |-> 0]
  /\ may = [p \in People |-> TRUE]
  /\ grant = [p \in People |-> TRUE]
  /\ entries = <<>>
  /\ flips = 0
  /\ turn = NoTurn
  /\ warm = NoProc
  /\ sess = [l \in Levels |-> EmptySess]
  /\ answered = {}
  /\ started = 0
  /\ restarts = 0
  /\ pend = {}
  /\ qnext = 0
  /\ intent = {}
  /\ snap = [p \in People |-> [armed |-> FALSE, open |-> FALSE, joined |-> 0]]
  /\ viol = {}
  /\ doorOut = {}
  /\ doors = 0

AddPerson(p) ==
  /\ ~open[p] /\ grant[p] /\ gen < MaxGen
  /\ Hold => ~turn.on
  /\ gen' = gen + 1
  /\ open' = [open EXCEPT ![p] = TRUE]
  /\ joined' = [joined EXCEPT ![p] = gen + 1]
  /\ ival' = [ival EXCEPT ![p] = @ + 1]
  /\ warm' = EndWarm
  /\ snap' = Disarm
  /\ UNCHANGED <<may, grant, entries, flips, turn, sess, answered, started, restarts,
                 pend, qnext, intent, viol>>

RemovePerson(p) ==
  /\ open[p] /\ gen < MaxGen
  /\ gen' = gen + 1
  /\ open' = [open EXCEPT ![p] = FALSE]
  /\ warm' = EndWarm
  /\ snap' = Disarm
  /\ UNCHANGED <<joined, ival, may, grant, entries, flips, turn, sess, answered,
                 started, restarts, pend, qnext, intent, viol>>

KaiPost ==
  /\ HasRoom
  /\ entries' = Append(entries, PostEntry(Kai, gen))
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, flips, turn, warm, sess, answered,
                 started, restarts, pend, qnext, intent, snap, viol>>

Revoke(p) ==
  /\ may[p]
  /\ flips < MaxFlips /\ flips' = flips + 1
  /\ may' = [may EXCEPT ![p] = FALSE]
  /\ snap' = [snap EXCEPT ![p] = [armed |-> TRUE, open |-> open[p], joined |-> joined[p]]]
  /\ open' = IF RevokeDeletes THEN [open EXCEPT ![p] = FALSE] ELSE open
  /\ UNCHANGED <<gen, joined, ival, grant, entries, turn, warm, sess, answered,
                 started, restarts, pend, qnext, intent, viol>>

Undo(p) ==
  /\ ~may[p]
  /\ flips < MaxFlips /\ flips' = flips + 1
  /\ may' = [may EXCEPT ![p] = TRUE]
  /\ viol' = IF snap[p].armed /\ (open[p] # snap[p].open \/ joined[p] # snap[p].joined)
             THEN Tag({"UndoExact"}) ELSE viol
  /\ snap' = [snap EXCEPT ![p].armed = FALSE]
  /\ UNCHANGED <<gen, open, joined, ival, grant, entries, turn, warm, sess,
                 answered, started, restarts, pend, qnext, intent>>

GrantDrop(p) ==
  /\ grant[p]
  /\ flips < MaxFlips /\ flips' = flips + 1
  /\ grant' = [grant EXCEPT ![p] = FALSE]
  /\ UNCHANGED <<gen, open, joined, ival, may, entries, turn, warm, sess, answered,
                 started, restarts, pend, qnext, intent, snap, viol>>

GrantBack(p) ==
  /\ ~grant[p]
  /\ flips < MaxFlips /\ flips' = flips + 1
  /\ grant' = [grant EXCEPT ![p] = TRUE]
  /\ UNCHANGED <<gen, open, joined, ival, may, entries, turn, warm, sess, answered,
                 started, restarts, pend, qnext, intent, snap, viol>>

Post(p) ==
  /\ ~SplitPost
  /\ open[p] /\ may[p] /\ HasRoom
  /\ entries' = Append(entries, PostEntry(p, gen))
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, flips, turn, warm, sess, answered,
                 started, restarts, pend, qnext, intent, snap, viol>>

PostCheck(p) ==
  /\ SplitPost
  /\ open[p] /\ may[p] /\ p \notin intent
  /\ intent' = intent \cup {p}
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, entries, flips, turn, warm, sess,
                 answered, started, restarts, pend, qnext, snap, viol>>

PostLand(p) ==
  /\ p \in intent /\ HasRoom
  /\ entries' = Append(entries, PostEntry(p, gen))
  /\ intent' = intent \ {p}
  /\ viol' = IF open[p] /\ may[p] THEN viol ELSE Tag({"R3w"})
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, flips, turn, warm, sess, answered,
                 started, restarts, pend, qnext, snap>>

QueuePost(p) ==
  /\ open[p] /\ may[p] /\ qnext < MaxQueued
  /\ pend' = pend \cup {[who |-> p, g |-> gen, iv |-> ival[p], id |-> qnext]}
  /\ qnext' = qnext + 1
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, entries, flips, turn, warm, sess,
                 answered, started, restarts, intent, snap, viol>>

DeliverQueued(q) ==
  /\ open[q.who] /\ may[q.who] /\ HasRoom
  /\ NoGenQueue \/ q.g = gen
  /\ entries' = Append(entries, PostEntry(q.who, IF NoGenQueue THEN gen ELSE q.g))
  /\ pend' = pend \ {q}
  /\ viol' = IF q.iv < ival[q.who] THEN Tag({"R4"}) ELSE viol
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, flips, turn, warm, sess, answered,
                 started, restarts, qnext, intent, snap>>

Deliver(p, i) ==
  /\ Visible(p, i)
  /\ LET e   == entries[i]
         bad == { t \in {"R1", "R1o", "R1f", "R3"} :
                  \/ (t = "R1"  /\ (e.level = "owner" \/ e.gen < joined[p]))
                  \/ (t = "R1o" /\ e.level = "owner")
                  \/ (t = "R1f" /\ (e.lo < joined[p] \/ e.own))
                  \/ (t = "R3"  /\ ~(open[p] /\ may[p])) }
     IN /\ bad \cap Track # {}
        /\ viol' = Tag(bad)
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, entries, flips, turn, warm, sess,
                 answered, started, restarts, pend, qnext, intent, snap>>

StartTurn ==
  /\ ~turn.on /\ started < MaxTurns
  /\ LET lvl   == IF Members = {} THEN "owner" ELSE "room"
         muted == lvl = "room" /\ \E p \in Members : ~may[p] \/ ~grant[p]
         inp   == {i \in Room :
                     /\ i \notin answered
                     /\ entries[i].author # Agent
                     /\ entries[i].gen <= gen
                     /\ FixInput => IF lvl = "owner" THEN entries[i].author \notin People
                                    ELSE entries[i].gen >= MaxJoined}
         reuse == warm.on /\ (ReuseIgnoresGen \/ (warm.gen = gen /\ warm.level = lvl))
         fresh == lvl = "room" /\ FreshSession /\ sess[lvl].gen # gen
         s     == IF fresh THEN [EmptySess EXCEPT !.gen = gen] ELSE sess[lvl]
         base  == IF reuse THEN warm
                  ELSE [on |-> TRUE, gen |-> gen, level |-> lvl, people |-> Members,
                        sgen |-> s.gen, lo |-> s.lo, guest |-> s.guest, own |-> s.own]
         pr    == Absorb(base, inp)
     IN /\ ~muted
        /\ inp # {}
        /\ turn' = [on |-> TRUE, gen |-> gen, level |-> lvl, input |-> inp,
                    startLen |-> Len(entries), proc |-> pr]
        /\ warm' = NoProc
        /\ sess' = [sess EXCEPT ![pr.level] = SessOf(pr)]
        /\ started' = started + 1
        /\ viol' = IF \/ (pr.level = "owner" /\ Members # {})
                      \/ (pr.level = "room" /\ ~(Members \subseteq pr.people))
                   THEN Tag({"R5"}) ELSE viol
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, entries, flips, answered, restarts,
                 pend, qnext, intent, snap>>

TurnSend ==
  /\ turn.on /\ HasRoom
  /\ entries' = Append(entries, Out(turn))
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, flips, turn, warm, sess, answered,
                 started, restarts, pend, qnext, intent, snap, viol>>

EndTurn ==
  /\ turn.on
  /\ entries' = IF HasRoom THEN Append(entries, Out(turn)) ELSE entries
  /\ answered' = answered \cup turn.input
  /\ warm' = turn.proc
  /\ turn' = NoTurn
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, flips, sess, started, restarts,
                 pend, qnext, intent, snap, viol>>

SteerIn ==
  /\ Steer /\ turn.on
  /\ \E i \in (turn.startLen + 1)..Len(entries) :
       /\ entries[i].author # Agent
       /\ i \notin turn.input
       /\ LET pr == Absorb(turn.proc, {i}) IN
            /\ turn' = [turn EXCEPT !.input = @ \cup {i}, !.proc = pr]
            /\ sess' = [sess EXCEPT ![pr.level] = SessOf(pr)]
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, entries, flips, warm, answered,
                 started, restarts, pend, qnext, intent, snap, viol>>

\* Doors run in their own processes, so a host restart does not lose a pending result.
HostRestart ==
  /\ restarts < MaxRestarts
  /\ restarts' = restarts + 1
  /\ turn' = NoTurn
  /\ warm' = NoProc
  /\ intent' = {}
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, entries, flips, sess, answered,
                 started, pend, qnext, snap, viol>>

(* ---------------- Doors (Q2 full) ---------------- *)
DoorStart ==
  /\ turn.on /\ doors < MaxDoors
  /\ doorOut' = doorOut \cup {[e |-> Out(turn), id |-> doors]}
  /\ doors' = doors + 1
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, entries, flips, turn, warm, sess,
                 answered, started, restarts, pend, qnext, intent, snap, viol>>

DoorLand(o) ==
  /\ HasRoom
  /\ entries' = Append(entries, IF DoorStampLand THEN [o.e EXCEPT !.gen = gen] ELSE o.e)
  /\ doorOut' = doorOut \ {o}
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, flips, turn, warm, sess,
                 answered, started, restarts, pend, qnext, intent, snap, viol, doors>>

OldNext ==
  \/ \E p \in People :
       \/ AddPerson(p) \/ RemovePerson(p) \/ Revoke(p) \/ Undo(p)
       \/ GrantDrop(p) \/ GrantBack(p)
       \/ Post(p) \/ PostCheck(p) \/ PostLand(p) \/ QueuePost(p)
       \/ \E i \in Room : Deliver(p, i)
  \/ \E q \in pend : DeliverQueued(q)
  \/ KaiPost
  \/ StartTurn \/ TurnSend \/ EndTurn \/ SteerIn
  \/ HostRestart

Next ==
  \/ (OldNext /\ UNCHANGED <<doorOut, doors>>)
  \/ DoorStart
  \/ \E o \in doorOut : DoorLand(o)

Spec == Init /\ [][Next]_vars

Perms == Permutations(People)

NoLeak          == "R1" \notin viol
NoOwnerLeak     == "R1o" \notin viol
NoLeakFlow      == "R1f" \notin viol
NoGuestInput ==
  /\ turn.on => \A i \in turn.input : entries[i].gen <= turn.gen
  /\ (turn.on /\ turn.level = "owner") => \A i \in turn.input : entries[i].author \notin People
  /\ (turn.on /\ turn.proc.level = "owner") => ~turn.proc.guest
  /\ (warm.on /\ warm.level = "owner") => ~warm.guest
  /\ ~sess["owner"].guest
Removed         == "R3" \notin viol
NoWriteWhileOut == "R3w" \notin viol
UndoExact       == "UndoExact" \notin viol
NoStaleLanding  == "R4" \notin viol
NoWiden         == "R5" \notin viol
=============================================================================
