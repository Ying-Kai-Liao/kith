---- MODULE RevokeDoors ----
(***************************************************************************)
(* Round 2 of the design model of designs/revoke-person.md (no code).     *)
(* Revoke.tla (round 1) let a Mac serve P "while offline". Wrong: a Mac    *)
(* has two doors with different reach.                                     *)
(*   APP door: every app message (P's app, B as a cloud bot guest) goes    *)
(*     through the Kith relay in the cloud. Needs the Mac online.          *)
(*   BRIDGE door (Telegram/Discord/Slack): the Mac talks to Telegram       *)
(*     directly. Works whenever the Mac has internet, Kith cloud or not.   *)
(*     Telegram queues P's messages while the Mac is offline (depth 1      *)
(*     here) and hands them over on reconnect.                             *)
(* Per Mac a network state: offline (no door), cloudDown (internet, no     *)
(* Kith cloud: bridge only, no cloud delivery), online (both doors).       *)
(* The owner revokes from the phone app (cloud record first, Macs learn    *)
(* by cloud delivery) or on a Mac's terminal (local flag at once, pushed   *)
(* to the cloud now if online, else from an outbox later). Undo the same.  *)
(* Revoke once, undo at most once. The cloud record carries a version;     *)
(* each Mac remembers the version it last applied.                         *)
(* A check and the act it guards are separate actions (start / helper     *)
(* re-check / deliver re-check) so a revoke can land between them.         *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
    \* ---- the narrow fix (TRUE in the design) ----
    RelayChecks,        \* the relay refuses app/bot traffic of P (and P's bots) whose cloud record says revoked,
                        \*   in the same step as the start, and again at delivery and helper spawn
    BridgeSyncFirst,    \* the bridge serves non-owners only while the Mac is in sync with the cloud (InSync)
    \* ---- who may revoke where ----
    OwnerCli,           \* TRUE = the owner may also revoke/undo on a Mac's terminal; FALSE = phone app only
    CliNeedsCloud,      \* TRUE = a terminal revoke/undo needs the Mac online (cloud written in the same step)
                        \* FALSE = a terminal revoke/undo works offline/cloudDown, pushed later from the outbox
    \* ---- broken variants from round 1 (FALSE in the design) ----
    StarIgnoresRevoke,  \* a `*` row answers before the revoked flag is looked at
    NoDeliverRecheck,   \* a turn's delivery re-checks nothing (no may(), no relay, no sync)
    RevokeDeletesRows,  \* revoke deletes P's rows on the Mac it lands on; undo only clears the flag
    BotIgnoresOwner,    \* B is allowed on its own row alone, without its owner's live may()
    HelperNoSync        \* a helper spawned from a bridge turn reads only may(), not whether the Mac is in sync

Macs    == {"m1", "m2"}
Who     == {"P", "B"}
Kinds   == {"hand", "host", "star"}
Nets    == {"offline", "cloudDown", "online"}
Doors   == {"none", "app", "bridge"}
\* P's slot: idle; queued = waiting in Telegram's queue (sent while the Mac was offline);
\* held = the bridge has it and waits until the Mac is in sync; then a turn.
Status  == {"idle", "queued", "held", "running", "helping", "delivered", "aborted"}
Waiting == {"idle", "queued", "held"}
Live    == {"running", "helping"}

Rows0  == [m \in Macs |-> IF m = "m1" THEN {"hand", "star"} ELSE {"hand", "host"}]
BotRow == [m \in Macs |-> TRUE]
BotMacs == {"m2"}   \* Macs B sends to (bound)

VARIABLES
    rows,          \* rows[m]: P's membership rows in m's auth.json
    revoked,       \* revoked[m]: the revoked flag on P in m's auth.json
    ver,           \* ver[m]: version of the cloud record m last applied (or wrote)
    outbox,        \* outbox[m]: m changed its flag without the cloud; not pushed yet (persisted)
    net,           \* net[m] \in Nets
    cloudRevoked,  \* the cloud's revoked-person record for P
    cloudVer,      \* its version: +1 on every write
    revokes,       \* 0..1
    undos,         \* 0..1
    turn,          \* turn[m][w] \in Status
    door           \* door[m]: the door of P's slot on m (B always uses the cloud bot door)

vars == <<rows, revoked, ver, outbox, net, cloudRevoked, cloudVer, revokes, undos, turn, door>>
auth == <<rows, revoked, ver, outbox, net, cloudRevoked, cloudVer, revokes, undos>>

(* ---- access ---- *)
MayP(m) == \/ StarIgnoresRevoke /\ "star" \in rows[m]
           \/ ~revoked[m] /\ rows[m] # {}
MayB(m) == BotRow[m] /\ (BotIgnoresOwner \/ MayP(m))
May(m, w) == IF w = "P" THEN MayP(m) ELSE MayB(m)

DoorOf(m, w) == IF w = "P" THEN door[m] ELSE "bot"

\* The relay's check, read in the same step as the act it guards.
RelayOK == ~RelayChecks \/ ~cloudRevoked
\* In sync: link to the cloud up, nothing unpushed, and m applied the cloud's latest record.
\* (A lease is the implementation; this reads the cloud's version in the same step.)
InSync(m) == net[m] = "online" /\ ~outbox[m] /\ ver[m] = cloudVer
BridgeGate(m) == ~BridgeSyncFirst \/ InSync(m)
\* Helper spawn re-check per door: app/bot ask the relay; the bridge needs sync.
HelperGate(m, w) == IF DoorOf(m, w) = "bridge"
                    THEN HelperNoSync \/ BridgeGate(m)
                    ELSE ~RelayChecks \/ (net[m] = "online" /\ ~cloudRevoked)

OwnerRevoked == revokes = 1 /\ undos = 0

\* A terminal revoke/undo on m: written to the cloud now if m is online, else kept in the outbox.
Publish(m, val) ==
    IF net[m] = "online"
    THEN /\ cloudRevoked' = val
         /\ cloudVer' = cloudVer + 1
         /\ ver' = [ver EXCEPT ![m] = cloudVer + 1]
         /\ outbox' = [outbox EXCEPT ![m] = FALSE]
    ELSE /\ outbox' = [outbox EXCEPT ![m] = TRUE]
         /\ UNCHANGED <<cloudRevoked, cloudVer, ver>>

Init ==
    /\ rows = Rows0
    /\ revoked = [m \in Macs |-> FALSE]
    /\ ver = [m \in Macs |-> 0]
    /\ outbox = [m \in Macs |-> FALSE]
    /\ net = [m \in Macs |-> "online"]
    /\ cloudRevoked = FALSE
    /\ cloudVer = 0
    /\ revokes = 0
    /\ undos = 0
    /\ turn = [m \in Macs |-> [w \in Who |-> "idle"]]
    /\ door = [m \in Macs |-> "none"]

(* ---- owner ---- *)
\* From the phone app: the cloud record first; Macs learn by cloud delivery.
AppRevoke ==
    /\ revokes = 0
    /\ revokes' = 1
    /\ cloudRevoked' = TRUE
    /\ cloudVer' = cloudVer + 1
    /\ UNCHANGED <<rows, revoked, ver, outbox, net, undos, turn, door>>

\* The app shows the cloud's record; undo is offered when it says revoked.
AppUndo ==
    /\ revokes = 1
    /\ undos = 0
    /\ cloudRevoked
    /\ undos' = 1
    /\ cloudRevoked' = FALSE
    /\ cloudVer' = cloudVer + 1
    /\ UNCHANGED <<rows, revoked, ver, outbox, net, revokes, turn, door>>

\* On m's terminal: local flag at once (rows untouched), cloud now or later.
CliRevoke(m) ==
    /\ OwnerCli
    /\ revokes = 0
    /\ ~CliNeedsCloud \/ net[m] = "online"
    /\ revokes' = 1
    /\ revoked' = [revoked EXCEPT ![m] = TRUE]
    /\ rows' = IF RevokeDeletesRows THEN [rows EXCEPT ![m] = {}] ELSE rows
    /\ Publish(m, TRUE)
    /\ UNCHANGED <<net, undos, turn, door>>

CliUndo(m) ==
    /\ OwnerCli
    /\ revokes = 1
    /\ undos = 0
    /\ revoked[m]
    /\ ~CliNeedsCloud \/ net[m] = "online"
    /\ undos' = 1
    /\ revoked' = [revoked EXCEPT ![m] = FALSE]
    /\ Publish(m, FALSE)
    /\ UNCHANGED <<rows, net, revokes, turn, door>>

(* ---- cloud and network ---- *)
\* An online Mac with an outbox pushes its flag; the cloud takes it as a new version (last writer wins).
Push(m) ==
    /\ net[m] = "online"
    /\ outbox[m]
    /\ cloudRevoked' = revoked[m]
    /\ cloudVer' = cloudVer + 1
    /\ ver' = [ver EXCEPT ![m] = cloudVer + 1]
    /\ outbox' = [outbox EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<rows, revoked, net, revokes, undos, turn, door>>

\* The cloud delivers its latest record to an online Mac that has nothing to push; kith-auth applies it.
CloudDeliver(m) ==
    /\ net[m] = "online"
    /\ ~outbox[m]
    /\ ver[m] # cloudVer
    /\ revoked' = [revoked EXCEPT ![m] = cloudRevoked]
    /\ ver' = [ver EXCEPT ![m] = cloudVer]
    /\ rows' = IF RevokeDeletesRows /\ cloudRevoked THEN [rows EXCEPT ![m] = {}] ELSE rows
    /\ UNCHANGED <<outbox, net, cloudRevoked, cloudVer, revokes, undos, turn, door>>

\* Internet lost, Kith cloud lost (or unreachable from m), or back.
SetNet(m, n) ==
    /\ net' = [net EXCEPT ![m] = n]
    /\ UNCHANGED <<rows, revoked, ver, outbox, cloudRevoked, cloudVer, revokes, undos, turn, door>>

(* ---- doors ---- *)
\* APP door: P's app message arrives through the relay. Relay check, Mac's may() and the start are one step.
AppStart(m) ==
    /\ turn[m]["P"] = "idle"
    /\ net[m] = "online"
    /\ RelayOK
    /\ MayP(m)
    /\ turn' = [turn EXCEPT ![m]["P"] = "running"]
    /\ door' = [door EXCEPT ![m] = "app"]
    /\ UNCHANGED auth

\* Cloud bot guest B: reaches the Mac only through the cloud, like the app door.
\* Bound: B sends only to m2 (its row is on both Macs; the m1 slot stays idle) to keep runs short.
BotStart(m) ==
    /\ m \in BotMacs
    /\ turn[m]["B"] = "idle"
    /\ net[m] = "online"
    /\ RelayOK
    /\ MayB(m)
    /\ turn' = [turn EXCEPT ![m]["B"] = "running"]
    /\ UNCHANGED <<auth, door>>

\* P writes on Telegram while m is offline: Telegram queues it.
TgQueue(m) ==
    /\ turn[m]["P"] = "idle"
    /\ net[m] = "offline"
    /\ turn' = [turn EXCEPT ![m]["P"] = "queued"]
    /\ door' = [door EXCEPT ![m] = "bridge"]
    /\ UNCHANGED auth

\* BRIDGE door: a message reaches the bridge (P writes now while m has internet, or Telegram hands over the
\* queued one on reconnect, or the bridge retries a held one). One step: out of sync -> hold it
\* ("can't check access right now"); in sync and may() -> the turn starts; in sync and not may() -> dropped
\* ("your access was removed").
BridgeRecv(m) ==
    /\ turn[m]["P"] \in Waiting
    /\ net[m] # "offline"
    /\ LET s == IF ~BridgeGate(m) THEN "held"
                ELSE IF MayP(m) THEN "running"
                ELSE "idle"
       IN /\ turn' = [turn EXCEPT ![m]["P"] = s]
          /\ door' = [door EXCEPT ![m] = IF s = "idle" THEN "none" ELSE "bridge"]
    /\ UNCHANGED auth

(* ---- a running turn ---- *)
\* Helper spawn re-checks may() (I2) and its door's gate. A refused spawn changes nothing.
Helper(m, w) ==
    /\ turn[m][w] = "running"
    /\ May(m, w)
    /\ HelperGate(m, w)
    /\ turn' = [turn EXCEPT ![m][w] = "helping"]
    /\ UNCHANGED <<auth, door>>

\* Delivery per door. App/bot: the reply goes through the relay, which re-checks the cloud record; the Mac
\* re-checks may(). Bridge: needs internet; holds the reply until in sync, then re-checks may().
\* A failed re-check drops the reply.
Deliver(m, w) ==
    /\ turn[m][w] \in Live
    /\ IF DoorOf(m, w) = "bridge"
       THEN /\ net[m] # "offline"
            /\ NoDeliverRecheck \/ BridgeGate(m)
            /\ turn' = [turn EXCEPT ![m][w] =
                           IF NoDeliverRecheck \/ May(m, w) THEN "delivered" ELSE "aborted"]
       ELSE /\ net[m] = "online"
            /\ turn' = [turn EXCEPT ![m][w] =
                           IF NoDeliverRecheck \/ (May(m, w) /\ RelayOK) THEN "delivered" ELSE "aborted"]
    /\ UNCHANGED <<auth, door>>

\* Optional: the door stops a live turn once the revoke reached m.
Stop(m, w) ==
    /\ turn[m][w] \in Live
    /\ revoked[m]
    /\ turn' = [turn EXCEPT ![m][w] = "aborted"]
    /\ UNCHANGED <<auth, door>>

\* Host restart (round 1's Restart) is left out here: it only aborts live turns, which no property depends on.

Next ==
    \/ AppRevoke
    \/ AppUndo
    \/ \E m \in Macs :
          \/ CliRevoke(m)
          \/ CliUndo(m)
          \/ Push(m)
          \/ CloudDeliver(m)
          \/ \E n \in Nets \ {net[m]} : SetNet(m, n)
          \/ AppStart(m)
          \/ BotStart(m)
          \/ TgQueue(m)
          \/ BridgeRecv(m)
    \/ \E m \in Macs, w \in Who :
          \/ Helper(m, w)
          \/ Deliver(m, w)
          \/ Stop(m, w)

Spec == Init /\ [][Next]_vars

(* ---- properties ---- *)
TypeOK ==
    /\ rows \in [Macs -> SUBSET Kinds]
    /\ revoked \in [Macs -> BOOLEAN]
    /\ ver \in [Macs -> 0..2]
    /\ outbox \in [Macs -> BOOLEAN]
    /\ net \in [Macs -> Nets]
    /\ cloudRevoked \in BOOLEAN
    /\ cloudVer \in 0..2
    /\ revokes \in 0..1
    /\ undos \in 0..1
    /\ turn \in [Macs -> [Who -> Status]]
    /\ door \in [Macs -> Doors]
    /\ \A m \in Macs : ver[m] <= cloudVer

\* Lemma the fix rests on: a Mac with nothing to push that has the cloud's latest version agrees with the cloud.
SyncAgrees == \A m \in Macs : (~outbox[m] /\ ver[m] = cloudVer) => revoked[m] = cloudRevoked

StartOf(m, w) == turn[m][w] \in Waiting /\ turn'[m][w] = "running"
ActOf(m, w)   == \/ turn[m][w] \in Live /\ turn'[m][w] = "delivered"
                 \/ turn[m][w] = "running" /\ turn'[m][w] = "helping"

\* R1: after P is revoked on a Mac, no new turn for P or B starts on that Mac.
R1 == [][\A m \in Macs, w \in Who : StartOf(m, w) => ~revoked[m]]_vars
\* R2: revoke beats every row.
R2 == \A m \in Macs : revoked[m] => ~MayP(m)
\* R4: wherever P is not revoked, P's rows are what they were.
R4 == \A m \in Macs : ~revoked[m] => rows[m] = Rows0[m]
\* R5: once the revoke reached a Mac, a turn there delivers nothing and spawns nothing.
R5 == [][\A m \in Macs, w \in Who : ActOf(m, w) => ~revoked[m]]_vars
\* R5c: once the cloud has the revoke, no Mac delivers to P/B or spawns for them.
R5c == [][\A m \in Macs, w \in Who : ActOf(m, w) => ~cloudRevoked]_vars
\* R6 (from the owner's act, until undo): no Mac starts a turn for P or B.
R6 == [][\A m \in Macs, w \in Who : StartOf(m, w) => ~OwnerRevoked]_vars
\* R6c: once the cloud has the revoke, no Mac starts a turn for P or B through any door.
R6c == [][\A m \in Macs, w \in Who : StartOf(m, w) => ~cloudRevoked]_vars
\* Two windows inside R6c, named so each gets its own counterexample:
\* the cloud-down window (the bridge starts P's turn while m has internet but no Kith cloud) ...
R6cDown == [][\A m \in Macs : (StartOf(m, "P") /\ door'[m] = "bridge" /\ net[m] = "cloudDown") => ~cloudRevoked]_vars
\* ... and the wake-up race (a message Telegram queued while m was offline starts once m is back online).
R6cWake == [][\A m \in Macs : (turn[m]["P"] = "queued" /\ turn'[m]["P"] = "running" /\ net[m] = "online") => ~cloudRevoked]_vars
\* B is never allowed where P is not.
BotBelowOwner == \A m \in Macs : MayB(m) => MayP(m)

(* ---- probes: each must FAIL on the design (a reachability witness), unless noted ---- *)
\* The bridge does serve P end to end when in sync and nothing is revoked.
NeverBridgeServed == \A m \in Macs : ~(turn[m]["P"] = "delivered" /\ door[m] = "bridge" /\ revokes = 0)
\* After an undo that m2 has applied, P gets an app turn on m2 again.
NeverAppAfterUndo == [][~(StartOf("m2", "P") /\ door'["m2"] = "app" /\ undos = 1 /\ ver["m2"] = 2)]_vars
\* A message Telegram queued while the Mac was offline is served after reconnect and sync, with the cloud
\* at version 2 (revoked and undone).
NeverQueuedServed == [][\A m \in Macs : ~(turn[m]["P"] = "queued" /\ turn'[m]["P"] = "running" /\ cloudVer = 2)]_vars
\* The wake-up race is met by waiting: on reconnect (online, but behind the cloud) the bridge holds a queued message.
NeverWakeHold == [][\A m \in Macs : ~(turn[m]["P"] = "queued" /\ turn'[m]["P"] = "held" /\ net[m] = "online")]_vars
\* ... and a held message is served once the Mac has applied the cloud's latest record (version 2: revoked, undone).
NeverHeldServed == [][\A m \in Macs : ~(turn[m]["P"] = "held" /\ turn'[m]["P"] = "running" /\ cloudVer = 2)]_vars
\* The cost: the bridge never holds P's message while nothing is revoked. Must FAIL (counterexample = the cost).
NoBridgeRefusal == revokes = 0 => \A m \in Macs : turn[m]["P"] # "held"
\* The bridge never starts P's turn while m is cloudDown and nothing is revoked.
\* PASSES on the design (in-sync needs the cloud link: the cost, proved); FAILS on BridgeNoSync (reachable).
NeverBridgeInCloudDown ==
    [][\A m \in Macs : ~(StartOf(m, "P") /\ door'[m] = "bridge" /\ net[m] = "cloudDown" /\ revokes = 0)]_vars
====
