------------------------------ MODULE Rooms2 ------------------------------
(***************************************************************************)
(* Rooms with people, slice 1, Shape B (design note 2026-11-12).           *)
(* One group room R on Kai's Mac (the host, R's one writer). Kai owns R.   *)
(* People join/leave as intervals in the host store; every room write is   *)
(* one host step (rule R6 is the single-writer assumption: one action =    *)
(* one host tx). The authority (may / planner grant) is a separate store,  *)
(* read live. One agent, planner. Entries are a sequence in the host store.*)
(*                                                                         *)
(* Flags: the design as written has all flags FALSE. FixInput and          *)
(* FreshSession are the fixes this model found needed (see results.md).    *)
(* Every other flag drops or changes one rule (a broken variant), or is    *)
(* the alternative design J-a (Hold).                                      *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS People, MaxEntries, MaxGen, MaxTurns, MaxRestarts, MaxQueued, MaxFlips,
          Track,           \* the property tags this run checks; other tags are not recorded
          FixInput,        \* owner turn: no person text; room turn: only what every current person sees
          FreshSession,    \* room-person session restarts at each gen (no carry-over across a join)
          NoTag,           \* variant notag: viewer filter is "member now" only
          Steer,           \* variant steer: entries appended after start are fed to the running turn
          ReuseIgnoresGen, \* variant warmreuse: warm process reused whatever its gen/level
          KeepWarmOnEdit,  \* variant warmreuse: add/remove does not end R's warm processes
          NoGenQueue,      \* variant nogen-queue: a queued post lands on "member now" only
          RevokeDeletes,   \* variant revokedeletes: Revoke(p) closes p's interval
          SplitPost,       \* variant split-post: Post is check, then later append
          Hold             \* design J-a: AddPerson waits until no turn is running

Kai    == "kai"
Agent  == "planner"
Levels == {"owner", "room"}
Inf    == MaxGen + 1

VARIABLES
  gen,       \* rooms.gen
  open,      \* p has an open interval in room_people
  joined,    \* joined_gen of p's latest interval
  ival,      \* ghost: number of p's intervals so far (interval id)
  may,       \* authority: may(p, agents of R)  (Revoke / Undo)
  grant,     \* authority: p is granted planner (GrantDrop / GrantBack)
  entries,   \* the room's entries, in host order
  flips,     \* authority changes so far (bound)
  turn,      \* the planner's in-flight turn (in memory)
  warm,      \* the planner's warm idle process for R (in memory)
  sess,      \* persisted sessions per identity: owner session, room-person session
  answered,  \* entries some finished turn took as input (persisted)
  started,   \* turns started (bound)
  restarts,  \* host restarts (bound)
  pend,      \* posts queued on a person's device, not yet landed
  qnext,     \* next queued-post id (bound)
  intent,    \* split-post only: people whose post passed the check, append pending (in memory)
  snap,      \* ghost: p's membership at their latest Revoke, armed until a room edit
  viol       \* ghost: tags of properties broken by an action

vars == <<gen, open, joined, ival, may, grant, entries, flips, turn, warm, sess,
          answered, started, restarts, pend, qnext, intent, snap, viol>>

(* A process's taint: lo = lowest gen of anything it has read; guest = it has
   read a person's text; own = it has read owner-level output. *)
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
\* Rule 1: an agent entry carries its turn's start gen; its level is the level
\* of the process that really produced it.
Out(t) == Entry(Agent, t.gen, t.proc.level, t.proc.lo, t.proc.guest,
                t.proc.own \/ t.proc.level = "owner")

Absorb(pr, S) ==
  [pr EXCEPT !.lo    = MinOf({pr.lo} \cup {entries[i].lo : i \in S}),
             !.guest = pr.guest \/ (\E i \in S : entries[i].guest),
             !.own   = pr.own   \/ (\E i \in S : entries[i].own)]
SessOf(pr) == [gen |-> pr.sgen, lo |-> pr.lo, guest |-> pr.guest, own |-> pr.own]

\* Rule 2 (viewer filter / live check at push time).
Visible(p, i) == /\ open[p] /\ may[p]
                 /\ (NoTag \/ joined[p] <= entries[i].gen)

\* Record a broken property only if this run checks it, so unchecked tags do
\* not split states.
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

(* ---------------- Kai (owner methods), one host tx each ---------------- *)
AddPerson(p) ==
  /\ ~open[p] /\ grant[p] /\ gen < MaxGen          \* rule 4: granted every agent in R
  /\ Hold => ~turn.on
  /\ gen' = gen + 1
  /\ open' = [open EXCEPT ![p] = TRUE]
  /\ joined' = [joined EXCEPT ![p] = gen + 1]
  /\ ival' = [ival EXCEPT ![p] = @ + 1]
  /\ warm' = EndWarm                                \* rule 3: end R's warm processes
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

(* ---------------- Authority (separate store, live) ---------------- *)
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

(* ---------------- A person ---------------- *)
\* Rule 1: check (open interval and may) and append in one tx, stamped with gen now.
Post(p) ==
  /\ ~SplitPost
  /\ open[p] /\ may[p] /\ HasRoom
  /\ entries' = Append(entries, PostEntry(p, gen))
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, flips, turn, warm, sess, answered,
                 started, restarts, pend, qnext, intent, snap, viol>>

PostCheck(p) ==               \* split-post: the check ...
  /\ SplitPost
  /\ open[p] /\ may[p] /\ p \notin intent
  /\ intent' = intent \cup {p}
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, entries, flips, turn, warm, sess,
                 answered, started, restarts, pend, qnext, snap, viol>>

PostLand(p) ==                \* ... and, later, the append
  /\ p \in intent /\ HasRoom
  /\ entries' = Append(entries, PostEntry(p, gen))
  /\ intent' = intent \ {p}
  /\ viol' = IF open[p] /\ may[p] THEN viol ELSE Tag({"R3w"})
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, flips, turn, warm, sess, answered,
                 started, restarts, pend, qnext, snap>>

\* A device queues a post (offline); it records the gen and interval it was in.
QueuePost(p) ==
  /\ open[p] /\ may[p] /\ qnext < MaxQueued
  /\ pend' = pend \cup {[who |-> p, g |-> gen, iv |-> ival[p], id |-> qnext]}
  /\ qnext' = qnext + 1
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, entries, flips, turn, warm, sess,
                 answered, started, restarts, intent, snap, viol>>

\* The queued post reaches the host, any time later (late). It stays pending
\* until it lands, so the device retries forever; the host dedups by id (twice).
DeliverQueued(q) ==
  /\ open[q.who] /\ may[q.who] /\ HasRoom
  /\ NoGenQueue \/ q.g = gen                         \* rules 1 / 8
  /\ entries' = Append(entries, PostEntry(q.who, IF NoGenQueue THEN gen ELSE q.g))
  /\ pend' = pend \ {q}
  /\ viol' = IF q.iv < ival[q.who] THEN Tag({"R4"}) ELSE viol
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, flips, turn, warm, sess, answered,
                 started, restarts, qnext, intent, snap>>

(* ---------------- Viewer fan-out (push / event / history) ---------------- *)
\* The host pushes entry i to p (event, push, history page). Enabled at any later
\* time and any number of times, so a late or repeated push is covered; after a
\* re-add it is re-checked. "p's delivered set in p's latest interval" is not
\* stored: entries never change and joined[p] changes only at AddPerson, so a
\* delivered entry breaks R1 iff it broke R1 at the moment it was pushed. The
\* push is therefore checked when it happens (same verdicts, far fewer states).
Deliver(p, i) ==
  /\ Visible(p, i)
  /\ LET e   == entries[i]
         bad == { t \in {"R1", "R1o", "R1f", "R3"} :
                  \/ (t = "R1"  /\ (e.level = "owner" \/ e.gen < joined[p]))
                  \/ (t = "R1o" /\ e.level = "owner")
                  \/ (t = "R1f" /\ (e.lo < joined[p] \/ e.own))
                  \/ (t = "R3"  /\ ~(open[p] /\ may[p])) }
     IN /\ bad \cap Track # {}      \* a clean push changes nothing: no need to step
        /\ viol' = Tag(bad)
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, entries, flips, turn, warm, sess,
                 answered, started, restarts, pend, qnext, intent, snap>>

(* ---------------- Turn runner (planner) ---------------- *)
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
     IN /\ ~muted                                   \* rule 4: muted, no start
        /\ inp # {}
        /\ turn' = [on |-> TRUE, gen |-> gen, level |-> lvl, input |-> inp,
                    startLen |-> Len(entries), proc |-> pr]
        /\ warm' = NoProc
        /\ sess' = [sess EXCEPT ![pr.level] = SessOf(pr)]   \* transcript written as it runs
        /\ started' = started + 1
        /\ viol' = IF \/ (pr.level = "owner" /\ Members # {})
                      \/ (pr.level = "room" /\ ~(Members \subseteq pr.people))
                   THEN Tag({"R5"}) ELSE viol
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, entries, flips, answered, restarts,
                 pend, qnext, intent, snap>>

TurnSend ==                   \* a tool / kith-send post mid-turn
  /\ turn.on /\ HasRoom
  /\ entries' = Append(entries, Out(turn))
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, flips, turn, warm, sess, answered,
                 started, restarts, pend, qnext, intent, snap, viol>>

EndTurn ==                    \* the reply; the process goes warm
  /\ turn.on
  /\ entries' = IF HasRoom THEN Append(entries, Out(turn)) ELSE entries
  /\ answered' = answered \cup turn.input
  /\ warm' = turn.proc
  /\ turn' = NoTurn
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, flips, sess, started, restarts,
                 pend, qnext, intent, snap, viol>>

SteerIn ==                    \* variant steer only
  /\ Steer /\ turn.on
  /\ \E i \in (turn.startLen + 1)..Len(entries) :
       /\ entries[i].author # Agent
       /\ i \notin turn.input
       /\ LET pr == Absorb(turn.proc, {i}) IN
            /\ turn' = [turn EXCEPT !.input = @ \cup {i}, !.proc = pr]
            /\ sess' = [sess EXCEPT ![pr.level] = SessOf(pr)]
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, entries, flips, warm, answered,
                 started, restarts, pend, qnext, intent, snap, viol>>

(* ---------------- Failures ---------------- *)
\* In-memory state lost (in-flight turn, warm process, a pending split-post);
\* host rows, entries, sessions, answered survive. The lost turn's input is still
\* unanswered, so it starts again (fresh start: new gen, level, input) without a
\* new charge (budget itself is not modeled).
HostRestart ==
  /\ restarts < MaxRestarts
  /\ restarts' = restarts + 1
  /\ turn' = NoTurn
  /\ warm' = NoProc
  /\ intent' = {}
  /\ UNCHANGED <<gen, open, joined, ival, may, grant, entries, flips, sess, answered,
                 started, pend, qnext, snap, viol>>

Next ==
  \/ \E p \in People :
       \/ AddPerson(p) \/ RemovePerson(p) \/ Revoke(p) \/ Undo(p)
       \/ GrantDrop(p) \/ GrantBack(p)
       \/ Post(p) \/ PostCheck(p) \/ PostLand(p) \/ QueuePost(p)
       \/ \E i \in Room : Deliver(p, i)
  \/ \E q \in pend : DeliverQueued(q)
  \/ KaiPost
  \/ StartTurn \/ TurnSend \/ EndTurn \/ SteerIn
  \/ HostRestart

Spec == Init /\ [][Next]_vars

Perms == Permutations(People)

(* ---------------- Properties ---------------- *)
\* R1: no owner-level entry, and none older than p's latest join, was delivered
\* to p in p's latest interval.
NoLeak == "R1" \notin viol
\* R1, owner half alone: no owner-level entry is ever pushed to a person.
NoOwnerLeak == "R1o" \notin viol

\* R1 (flow, stronger): nothing delivered to p was made from anything older than
\* p's latest join, or from owner-level output. ("answer with only what every
\* person in the room may see".)
NoLeakFlow == "R1f" \notin viol

\* R2: no person's text and nothing newer than the turn reaches an owner-level
\* turn, its process, or the owner session.
NoGuestInput ==
  /\ turn.on => \A i \in turn.input : entries[i].gen <= turn.gen
  /\ (turn.on /\ turn.level = "owner") => \A i \in turn.input : entries[i].author \notin People
  /\ (turn.on /\ turn.proc.level = "owner") => ~turn.proc.guest
  /\ (warm.on /\ warm.level = "owner") => ~warm.guest
  /\ ~sess["owner"].guest

Removed         == "R3" \notin viol          \* R3: nothing new while removed or revoked
NoWriteWhileOut == "R3w" \notin viol         \* R3 (write side): no post lands while out
UndoExact       == "UndoExact" \notin viol   \* R3: undo restores membership exactly
NoStaleLanding  == "R4" \notin viol          \* R4
NoWiden         == "R5" \notin viol          \* R5 (at start; never-widens-mid-turn: see results)
=============================================================================
