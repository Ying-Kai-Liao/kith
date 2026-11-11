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
(* ROUND 4 (revoke/Stop sweeps, designs/2026-10-27-kith-plan step 3): three more *)
(*   things a revoke does, each behind a switch so the older rows stay small.  *)
(*   ItemsOn: P's devices, memberships and open invites are PARKED by a revoke  *)
(*     (nothing grants access) and come back with the undo, exactly.            *)
(*   Asks: P's turn starts a delegation (`kith ask`); a revoke ENDS it (record  *)
(*     stopped, run killed) and withdraws the wake its answer would be; a      *)
(*     stopped ask never wakes anyone (Stop beats the answer, R8).             *)
(*   HandleSync: a person known only by a chat handle is revoked/undone on one *)
(*     Mac and the other (paired, maybe off) takes it over Mac link, the       *)
(*     latest stamp winning; nothing goes to the cloud.                        *)
(* GUEST DOOR (round 3, guest workbench): a turn of P may run a declared   *)
(*   door (video's render) outside its sandbox, once, on m1 here. Its      *)
(*   start is one step under kith-auth's lock (D2: allowed, not held =     *)
(*   in sync); the host's sweep kills it once P may no longer run it (D1). *)
(* ALWAYS (C-a, 2026-11-12): the escape card's "always" (kith-auth access --by; the core's    *)
(*   people.access, core/runtime/src/authority/card.rs) writes an entry on P's row, or on B's,  *)
(*   in ONE locked step guarded by may() now, a bot only within what its owner holds, and a     *)
(*   self-approval only while P is a member. A demotion drops the entries P approved             *)
(*   (drop_self_approved, M3). Removals take the entry as written (AlwaysRemove).               *)
(* BOT DOOR (N2, 2026-11-01): B's own tick may name a door its owner P     *)
(*   was not given; the door check cuts B's grants to P's (kith-auth       *)
(*   owner_cut, the same cut resolve_bot makes), so B never starts a door  *)
(*   P can't (I6).                                                         *)
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
    HelperNoSync,       \* a helper spawned from a bridge turn reads only may(), not whether the Mac is in sync
    \* ---- the guest door ----
    GuestDoor,          \* TRUE = P's guest door is in the model. Nothing else reads gdoor, so the rows of the other
                        \*   properties leave it out (FALSE in base.consts) and stay small; the door rows turn it on
    \* ---- broken guest doors (FALSE in the design) ----
    DoorNoHold,         \* the door start reads may() only, not whether the Mac is in sync (GuestDoor.tla "nohold")
    DoorSplit,          \* the door checks, then claims and starts in a later step (GuestDoor.tla "split")
    NoDoorSweep,        \* nothing kills a running door when its person is revoked
    BotDoor,            \* TRUE = B's guest door and P's tick of its job are in the model (FALSE in base.consts)
    BotDoorNoCut,       \* B's door check reads B's own tick only, not cut to what P holds (kith-auth owner_cut)
    \* ---- the core's door table (M2-14, core/runtime/src/doors) ----
    CoreDoorHook,       \* TRUE = the revoke that lands on a Mac (people.suspend, kith-auth --local) also marks P's and
                        \*   B's door ROWS there swept, in the same transaction (FALSE = only the sweep action does)
    DoorUndoRevives,    \* an undo puts a swept door back to running (the core's undo touches no door row)
    \* ---- round 4 ----
    ItemsOn,            \* TRUE = P's devices, memberships and invites are in the model
    DeleteItems,        \* a revoke deletes them instead of parking them (the old people.rs / members.rs end_access)
    Asks,               \* TRUE = P's delegations and the wakes their answers make are in the model
    NoAskSweep,         \* a revoke leaves P's open delegations running
    CoreSweep,          \* TRUE = the core's claim sweep (conversations::claims::revoke_person for P and B, M3-10d) is in
                        \*   the model, and the asks are ended only after it (revoke_order.rs); FALSE = no claim sweep. Triggered by a removal and by a suspend in the code
                        \*   (people.suspend records the order and runs it after the commit)
    AsksFirst,          \* the asks are ended before the claims are swept (the wrong order)
    KillSplit,          \* TRUE = the claim sweep is two steps: ClaimEnding (the core commits the claim to `ending`) and KillSent
                        \*   (the host sends the signal once, after the commit); FALSE = one step that also stops the process
    HostCrash,          \* the host may die, or its spawn fail, between ClaimEnding and KillSent: the signal is lost (`stuck`)
    LinkFlaps,          \* the holder link on a Mac may go down (and stay down): a remote, absent or failing holder
    ReSendNeedsLink,    \* the WRONG design: the re-send runs inside the holder link, so only while it is up
    ReSendKills,        \* the daemon's supervisor pass signals every `ending` claim whose process matches its start stamp
                        \*   and whose end_reason is a kill's (not done / exit / out_of_usage, kill_resend.rs)
    NamelessReport,     \* a live claim's stub reports done/exit naming no pid: the claim goes `ending` with that reason while the
                        \*   process may still run (`reported`, claims.rs apply)
    RevokeKeepsReason,  \* BROKEN (before L1012): the revoke lists a `reported` claim's kill but leaves its reason, so the re-send skips it
    SkipBots,           \* the claim sweep covers P's own claims only, not B's (the bot principal holds its own)
    NoWakeWithdraw,     \* a revoke leaves the wake owed to P's session
    AnswerIgnoresEnd,   \* an answer to an ended (stopped) ask is still kept and wakes its caller
    HandleSync,         \* TRUE = a handle-only person Q, revoked on one Mac, and the second Mac starts off
    HandleNoStamp,      \* a peer's state overrides this one's whatever the stamps say
    HandleNoCatchup,    \* a handle revoke is sent only at the moment it is made, to peers then online
    HandleSkew,         \* m2's clock reads behind the stamps it has been sent (clocks disagree)
    HandleNoBump,       \* a change is stamped with the clock alone, so it can be older than the stamp kept (and is dropped)
    HandleFutureOk,     \* a peer's stamp from the far future is taken as it is
    HandleStoreFirst,   \* a peer's stamp is kept before the change is applied, so a failed apply is never retried
    HandleApplyFails,   \* applying a peer's change can fail (kith-auth refuses, or there is none)
    Removal,            \* TRUE = the owner can remove P while P is revoked
    RemoveKeeps,        \* a removal leaves what the revoke parked (device keys, invite token hashes) behind
    \* ---- the core's cards (M2-11b, core/runtime/src/cards, authority/revoke.rs follow_in_cards) ----
    CardsOn,            \* TRUE = one card of P's, asked on m1 before any revoke, and the yes that becomes a once grant
    CardNoWithdraw,     \* the revoke leaves P's open cards and unused grants as they were (before M2-11b)
    CardUndoRevives,    \* an undo puts a withdrawn card back (the core's undo touches no card)
    \* ---- the core's authority behind the host's request guard (M2-15/16/18a, KITH_CORE_AUTHORITY=1; OFF in every deploy) ----
    CoreGate,           \* TRUE = the host's guard (app and bot doors) asks the core's copy of the revoked fact while the
                        \*   gate is open, and kith-auth's mirror writes the core after auth.json (FALSE = the host asks kith-auth)
    CoreGateStale,      \* the gate stays open on a latched answer after auth.json changed (no fingerprint check)
    \* ---- the switched host (#875: auth.json is a tombstone; the core is the only store) ----
    Switched,           \* (CliRevoke/CliUndo stand for the host's local_revoke; the terminal's `kith person --revoke` exits 3 on a tombstone
                        \*   and `person` is not in core-switched.txt: M2-18 gate 1, open) TRUE = authority is on the core: the host's kith-auth calls are people.* writes (run_auth), the guard asks
                        \*   the core with no gate in between (core_selected = switched() || ...), a revoke changes the one fact
    SwitchLag,          \* BROKEN: the host's cached switched() (2 s) outlives a rollback: the revoke goes to the core only, the file
                        \*   (now kith-auth's again) never gets it, and the guard is back on kith-auth
    CoreOnlyWrite,      \* the core's REVOKED FACT can change with no write of auth.json (a hand-run core-call people.suspend/undo). The
                        \*   access-request writes (decide_in_core, deny_or_queue, fileRequest in cloud.rs; core_grant/core_revoke in people.rs) are
                        \*   core-only too, but they write agent rows and requests, never this fact
    CoreSideShutsGate,  \* ... and the gate sees it at once (parity checked per request); FALSE = the latch stays open until its next ask
    \* ---- the card writes behind the router (C-a, 2026-11-12): kith-auth on the tombstone writes through the core ----
    AlwaysOn,           \* TRUE = the escape card's always (P's row and B's) and the demotion that drops P's approvals are in the model
    RouterDoor,         \* TRUE = the always is the core's write (people.access): it reads the core's fact in its own locked step
    AlwaysNoMay,        \* BROKEN: the always is written with no may() in its step (the check was dropped)
    AlwaysSplit,        \* BROKEN: the always checks may() in one step and writes in a later one (a revoke can land between)
    DemoteKeeps,        \* BROKEN: a demotion leaves the entries the demoted member approved (no drop_self_approved)
    GrantOn,            \* TRUE = the secret grant for P's own account (agent@P, S-b) is in the model
    GrantNoPerson,      \* BROKEN: the grant is written with no need_person(P) in its step (a removed P gets one)
    ForgetKeepsGrant    \* BROKEN: removing P leaves the grants made for P (no forget)

ASSUME (Switched => CoreGate) /\ (SwitchLag => Switched) /\ (CoreSideShutsGate => CoreOnlyWrite)
ASSUME NamelessReport => KillSplit
ASSUME (RouterDoor => (Switched /\ AlwaysOn)) /\ ((AlwaysNoMay \/ AlwaysSplit \/ DemoteKeeps) => AlwaysOn)
ASSUME (GrantNoPerson \/ ForgetKeepsGrant) => GrantOn

Macs    == {"m1", "m2"}
Who     == {"P", "B"}
Kinds   == {"hand", "host", "star"}
Nets    == {"offline", "cloudDown", "online"}
Doors   == {"none", "app", "bridge"}
\* P's slot: idle; queued = waiting in Telegram's queue (sent while the Mac was offline);
\* held = the bridge has it and waits until the Mac is in sync; then a turn.
Status  == {"idle", "queued", "held", "running", "helping", "delivered", "aborted", "ending", "stuck",
            "reported", "rending", "rstuck"}
Waiting == {"idle", "queued", "held"}
Live    == {"running", "helping"}
\* A process still alive behind a claim the core has already moved to `ending` (`stuck`: its one kill was lost).
\* reported: `ending` with a done/exit reason (NamelessReport), process alive; rending / rstuck: the same row after a
\* revoke that kept that reason (RevokeKeepsReason), its one kill pending / lost.
Proc    == Live \cup {"ending", "stuck", "reported", "rending", "rstuck"}
\* P's guest door on a Mac: idle; checked (DoorSplit only: access read, no claim yet); running (claimed, so the
\* sweep sees it); killed by the sweep; done (it ended by itself).
GDoor   == {"idle", "checked", "running", "killed", "done"}

Rows0  == [m \in Macs |-> IF m = "m1" THEN {"hand", "star"} ELSE {"hand", "host"}]
BotRow == [m \in Macs |-> TRUE]
BotMacs == {"m2"}   \* Macs B sends to (bound)
DoorMacs == IF GuestDoor THEN {"m1"} ELSE {}   \* Macs P runs a guest door on (bound; m2 stays idle)

Items0  == {"member", "invite", "device"}

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
    door,          \* door[m]: the door of P's slot on m (B always uses the cloud bot door)
    gdoor,         \* gdoor[m] \in GDoor: P's guest door on m
    items,         \* items[m]: P's devices, memberships and open invites that grant access on m
    parked,        \* parked[m]: the ones a revoke kept for the undo
    gone,          \* gone[m]: P was removed on m (while revoked)
    ask,           \* ask[m] \in AskState: P's delegation on m
    wakeOwed,      \* wakeOwed[m]: the answer of P's ask is owed to P's session as a wake
    hrev,          \* hrev[m]: the handle-only person Q is revoked on m
    hat,           \* hat[m]: the stamp of that fact
    clk,           \* a clock for the stamps
    hops,          \* handle changes made (0..2)
    bdoor,         \* bdoor[m] \in GDoor: B's guest door on m (its own tick names the door)
    cardst,        \* cardst[m] \in CardSt: P's card on m (asked, answered yes, picked up, or withdrawn)
    cardhit,       \* cardhit[m]: a revoke has landed on m while the card was open or unused (the oracle; no step reads it)
    crev,          \* crev[m].v: the core's copy of P's revoked fact on m (people.suspend / people.undo), written after auth.json;
                   \*   crev[m].side: a core-only write left the gate's latch open on it (until the gate's next parity ask)
    link,          \* link[m]: a local holder is connected on m (the daemon's tick does not need it)
    ptick,         \* P holds the door's job (P's tick on P's own row), given and taken back by the owner
    alw            \* alw[m]: P's always entries on m. mem: P is a member of the agent now; e[w] = who approved the entry on w's row
                   \*   ("none", the owner, or P as a member: "self"); chk[w]: the split variant's check, read earlier; bad[w]: the oracle
                   \*   (no step reads it): an entry was written while may() was false

CardSt == {"none", "open", "approved", "used", "withdrawn"}
AskState == {"none", "open", "answered", "stopped"}

vars == <<rows, revoked, crev, ver, outbox, net, cloudRevoked, cloudVer, revokes, undos, turn, door, gdoor,
          items, parked, gone, cardst, cardhit, ask, wakeOwed, hrev, hat, clk, hops, bdoor, ptick, alw, link>>
nolink == <<rows, revoked, crev, ver, outbox, net, cloudRevoked, cloudVer, revokes, undos, turn, door, gdoor,
            items, parked, gone, cardst, cardhit, ask, wakeOwed, hrev, hat, clk, hops, bdoor, ptick, alw>>
ext4 == <<items, parked, gone, cardst, cardhit, ask, wakeOwed, hrev, hat, clk, hops>>
ext == <<ext4, bdoor, ptick, alw, link>>
auth == <<rows, revoked, crev, ver, outbox, net, cloudRevoked, cloudVer, revokes, undos>>

(* ---- access ---- *)
MayP(m) == \/ StarIgnoresRevoke /\ "star" \in rows[m]
           \/ ~revoked[m] /\ rows[m] # {}
MayB(m) == BotRow[m] /\ (BotIgnoresOwner \/ MayP(m))
May(m, w) == IF w = "P" THEN MayP(m) ELSE MayB(m)

\* The host's request guard (api/roles.rs resolve -> api/authority.rs core_verdict), which the app door's start, the bot
\* door's start and a bot's reply (botThread) ask. With KITH_CORE_AUTHORITY=1 it asks the core's copy, but only while the
\* gate is open: the gate latches the core's parity answer with auth.json's fingerprint, and any write of auth.json (a new
\* inode) shuts it at once, so the host is back on kith-auth until the core has been told (core_selected). The model reads
\* the parity in the same step as the check. That is sound because fingerprint_of covers auth.json only and the core's revoked fact changes
\* only through mirror_to_core after save_auth under the same flock. A core-only write (a hand-run core-call; no production
\* writer: switched, the host writes the core and nothing else) leaves the latch open and out of step until the gate thread asks again
\* (GATE_EVERY, 2 s; GATE_MAX_AGE, 6 s, only if that thread hangs): CoreOnlyWrite and `crev[m].side` model it. The core's own answer is the same rule: a revoked person is refused before any row.
\* Switched (#875): no gate at all. core_selected() is switched() first, and the core is the only store, so the answer is the
\* core's own read per request (the host caches only whether the file is a tombstone, by its fingerprint, for 2 s).
\* The core-only writer above exists only unswitched (rows core-only-write*): switched there is no latch.
GateOpen(m) == CoreGate /\ ~SwitchLag /\ (Switched \/ CoreGateStale \/ crev[m].v = revoked[m] \/ crev[m].side)
GuardP(m) == IF GateOpen(m) THEN ~crev[m].v /\ rows[m] # {} ELSE MayP(m)
GuardB(m) == BotRow[m] /\ (BotIgnoresOwner \/ GuardP(m))
GuardMay(m, w) == IF w = "P" THEN GuardP(m) ELSE GuardB(m)

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
         /\ UNCHANGED <<cloudRevoked, cloudVer, ver, link>>

\* A revoke landing on m (people.rs cut_off -> store.suspend_person): what grants access is parked, not deleted. Landing
\* twice changes nothing more. The undo (restore_person) gives back exactly what was parked.
ItemsRevoke(m) ==
    IF ~ItemsOn \/ revoked[m]
    THEN UNCHANGED <<items, parked, gone, link>>
    ELSE /\ items' = [items EXCEPT ![m] = {}]
         /\ parked' = [parked EXCEPT ![m] = IF DeleteItems THEN {} ELSE items[m]]
         /\ gone' = gone
ItemsUndo(m) ==
    IF ~ItemsOn \/ ~revoked[m]
    THEN UNCHANGED <<items, parked, gone, link>>
    ELSE /\ items' = [items EXCEPT ![m] = items[m] \union parked[m]]
         /\ parked' = [parked EXCEPT ![m] = {}]
         /\ gone' = gone
ItemsStep(m, val) == IF val THEN ItemsRevoke(m) ELSE ItemsUndo(m)

Init ==
    /\ rows = Rows0
    /\ revoked = [m \in Macs |-> FALSE]
    /\ crev = [m \in Macs |-> [v |-> FALSE, side |-> FALSE]]
    /\ ver = [m \in Macs |-> 0]
    /\ outbox = [m \in Macs |-> FALSE]
    /\ net = [m \in Macs |-> IF HandleSync /\ m = "m2" THEN "offline" ELSE "online"]
    /\ cloudRevoked = FALSE
    /\ cloudVer = 0
    /\ revokes = 0
    /\ undos = 0
    /\ turn = [m \in Macs |-> [w \in Who |-> "idle"]]
    /\ door = [m \in Macs |-> "none"]
    /\ gdoor = [m \in Macs |-> "idle"]
    /\ items = [m \in Macs |-> Items0]
    /\ parked = [m \in Macs |-> {}]
    /\ gone = [m \in Macs |-> FALSE]
    /\ ask = [m \in Macs |-> "none"]
    /\ wakeOwed = [m \in Macs |-> FALSE]
    /\ hrev = [m \in Macs |-> FALSE]
    /\ hat = [m \in Macs |-> 0]
    /\ clk = 0
    /\ hops = 0
    /\ bdoor = [m \in Macs |-> "idle"]
    /\ ptick = FALSE
    /\ link = [m \in Macs |-> TRUE]
    /\ cardst = [m \in Macs |-> "none"]
    /\ cardhit = [m \in Macs |-> FALSE]
    /\ alw = [m \in Macs |-> [mem |-> TRUE, e |-> [w \in Who |-> "none"], chk |-> [w \in Who |-> "none"],
                              bad |-> [w \in Who |-> FALSE], gr |-> FALSE, grbad |-> FALSE]]

(* ---- owner ---- *)
\* From the phone app: the cloud record first; Macs learn by cloud delivery.
AppRevoke ==
    /\ revokes = 0
    /\ revokes' = 1
    /\ cloudRevoked' = TRUE
    /\ cloudVer' = cloudVer + 1
    /\ UNCHANGED <<rows, revoked, crev, ver, outbox, net, undos, turn, door, gdoor, ext>>

\* The app shows the cloud's record; undo is offered when it says revoked.
AppUndo ==
    /\ revokes = 1
    /\ undos = 0
    /\ cloudRevoked
    /\ undos' = 1
    /\ cloudRevoked' = FALSE
    /\ cloudVer' = cloudVer + 1
    /\ UNCHANGED <<rows, revoked, crev, ver, outbox, net, revokes, turn, door, gdoor, ext>>

\* The core's revoke hook (authority/revoke.rs follow_in_doors): in the transaction that sets the revoked fact on m, the
\* rows of P's and B's running doors there become swept ("killed" here: the ROW is swept; the box is stopped by whoever
\* reads the `doors` answer, which nothing does until the host is switched: a precondition of the switch, and what DoorD1
\* rests on). Undo calls nothing, and a swept row is final.
DoorsSwept(d, m, nowRevoked) ==
    IF CoreDoorHook /\ nowRevoked /\ d[m] = "running" THEN [d EXCEPT ![m] = "killed"] ELSE d
\* The broken variant of undo: it brings a swept door back.
DoorsUndone(d, m, nowRevoked) ==
    IF DoorUndoRevives /\ ~nowRevoked /\ d[m] = "killed" THEN [d EXCEPT ![m] = "running"] ELSE d

\* The core's card hook (authority/revoke.rs follow_in_cards, M2-11b): in the transaction that sets the revoked fact on m,
\* P's and B's open cards and unused once-grants there are withdrawn. Undo calls nothing, and a withdrawn card is final.
\* `cardhit` is the oracle: a revoke landed while the card was still worth something.
CardsAfter(m, nowRevoked) ==
    IF nowRevoked /\ ~CardNoWithdraw /\ cardst[m] \in {"open", "approved"} THEN [cardst EXCEPT ![m] = "withdrawn"]
    ELSE IF ~nowRevoked /\ CardUndoRevives /\ cardst[m] = "withdrawn" THEN [cardst EXCEPT ![m] = "approved"]
    ELSE cardst
CardHitAfter(m, nowRevoked) ==
    IF nowRevoked /\ cardst[m] \in {"open", "approved"} THEN [cardhit EXCEPT ![m] = TRUE] ELSE cardhit

\* Switched, the write the revoke makes IS the core's: kith-auth's file and the core's copy are one fact (run_auth ->
\* people.suspend / people.undo). Not under SwitchLag: the revoke then reaches the core only.
CrevAfter(m, v) == IF Switched THEN [crev EXCEPT ![m] = [v |-> v, side |-> FALSE]]
                   ELSE [crev EXCEPT ![m].side = FALSE]   \* a write of auth.json moves its fingerprint: the latch is shut
RevokedAfter(m, v) == IF SwitchLag THEN revoked ELSE [revoked EXCEPT ![m] = v]

\* On m's terminal: local flag at once (rows untouched), cloud now or later.
CliRevoke(m) ==
    /\ OwnerCli
    /\ revokes = 0
    /\ ~CliNeedsCloud \/ net[m] = "online"
    /\ revokes' = 1
    /\ revoked' = RevokedAfter(m, TRUE)
    /\ crev' = CrevAfter(m, TRUE)
    /\ rows' = IF RevokeDeletesRows THEN [rows EXCEPT ![m] = {}] ELSE rows
    /\ Publish(m, TRUE)
    /\ ItemsRevoke(m)
    /\ gdoor' = DoorsSwept(gdoor, m, TRUE)
    /\ bdoor' = DoorsSwept(bdoor, m, TRUE)
    /\ cardst' = CardsAfter(m, TRUE)
    /\ cardhit' = CardHitAfter(m, TRUE)
    /\ UNCHANGED <<net, undos, turn, door, ask, wakeOwed, hrev, hat, clk, hops, ptick, alw, link>>

CliUndo(m) ==
    /\ OwnerCli
    /\ revokes = 1
    /\ undos = 0
    /\ revoked[m]
    /\ ~CliNeedsCloud \/ net[m] = "online"
    /\ undos' = 1
    /\ revoked' = RevokedAfter(m, FALSE)
    /\ crev' = CrevAfter(m, FALSE)
    /\ Publish(m, FALSE)
    /\ ItemsUndo(m)
    /\ gdoor' = DoorsUndone(gdoor, m, FALSE)
    /\ bdoor' = DoorsUndone(bdoor, m, FALSE)
    /\ cardst' = CardsAfter(m, FALSE)
    /\ cardhit' = cardhit
    /\ UNCHANGED <<rows, net, revokes, turn, door, ask, wakeOwed, hrev, hat, clk, hops, ptick, alw, link>>

\* kith-auth's mirror (kernel/bin/kith-auth mirror_to_core, after save_auth; the embedded core when no daemon runs): the core's
\* copy takes the revoked fact once auth.json has it. One try, no queue; a try that fails (a warning, exit 0) is the step not taken,
\* and the gate then stays shut until the core is told (`core-migrate`, or the owner runs the command again).
CoreMirror(m) ==
    /\ CoreGate
    /\ ~Switched   \* switched there is no second copy to catch up
    /\ crev[m].v # revoked[m]
    /\ crev' = [crev EXCEPT ![m] = [v |-> revoked[m], side |-> FALSE]]
    /\ UNCHANGED <<rows, revoked, ver, outbox, net, cloudRevoked, cloudVer, revokes, undos, turn, door, gdoor, ext>>

\* A write of the core's revoked fact that never touches auth.json (a hand-run `kith kernel core-call people.undo`): the fingerprint
\* does not move, so the latch stays open until the gate thread asks the parity again (up to GATE_MAX_AGE).
CoreSide(m) ==
    /\ CoreOnlyWrite
    /\ CoreGate
    /\ crev' = [crev EXCEPT ![m] = [v |-> ~crev[m].v, side |-> ~CoreSideShutsGate]]
    /\ UNCHANGED <<rows, revoked, ver, outbox, net, cloudRevoked, cloudVer, revokes, undos, turn, door, gdoor, ext>>

\* The gate thread asks the parity again (every GATE_EVERY): a latch open on an out-of-step core shuts.
GateAsk(m) ==
    /\ CoreOnlyWrite
    /\ crev[m].side
    /\ crev' = [crev EXCEPT ![m].side = FALSE]
    /\ UNCHANGED <<rows, revoked, ver, outbox, net, cloudRevoked, cloudVer, revokes, undos, turn, door, gdoor, ext>>

(* ---- cloud and network ---- *)
\* An online Mac with an outbox pushes its flag; the cloud takes it as a new version (last writer wins).
Push(m) ==
    /\ net[m] = "online"
    /\ outbox[m]
    /\ cloudRevoked' = revoked[m]
    /\ cloudVer' = cloudVer + 1
    /\ ver' = [ver EXCEPT ![m] = cloudVer + 1]
    /\ outbox' = [outbox EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<rows, revoked, crev, net, revokes, undos, turn, door, gdoor, ext>>

\* The cloud delivers its latest record to an online Mac that has nothing to push; kith-auth applies it.
CloudDeliver(m) ==
    /\ net[m] = "online"
    /\ ~outbox[m]
    /\ ver[m] # cloudVer
    /\ revoked' = RevokedAfter(m, cloudRevoked)
    /\ crev' = CrevAfter(m, cloudRevoked)
    /\ ver' = [ver EXCEPT ![m] = cloudVer]
    /\ rows' = IF RevokeDeletesRows /\ cloudRevoked THEN [rows EXCEPT ![m] = {}] ELSE rows
    /\ ItemsStep(m, cloudRevoked)
    /\ gdoor' = DoorsUndone(DoorsSwept(gdoor, m, cloudRevoked), m, cloudRevoked)
    /\ bdoor' = DoorsUndone(DoorsSwept(bdoor, m, cloudRevoked), m, cloudRevoked)
    /\ cardst' = CardsAfter(m, cloudRevoked)
    /\ cardhit' = CardHitAfter(m, cloudRevoked)
    /\ UNCHANGED <<outbox, net, cloudRevoked, cloudVer, revokes, undos, turn, door, ask, wakeOwed, hrev, hat, clk, hops, ptick, alw, link>>

\* Internet lost, Kith cloud lost (or unreachable from m), or back.
SetNet(m, n) ==
    /\ ~HandleSync \/ (m = "m2" /\ n = "online")   \* handle rows: m2 starts off and comes on once
    /\ net' = [net EXCEPT ![m] = n]
    /\ UNCHANGED <<rows, revoked, crev, ver, outbox, cloudRevoked, cloudVer, revokes, undos, turn, door, gdoor, ext>>

(* ---- doors ---- *)
\* APP door: P's app message arrives through the relay. Relay check, Mac's may() and the start are one step.
AppStart(m) ==
    /\ turn[m]["P"] = "idle"
    /\ net[m] = "online"
    /\ RelayOK
    /\ GuardP(m)
    /\ turn' = [turn EXCEPT ![m]["P"] = "running"]
    /\ door' = [door EXCEPT ![m] = "app"]
    /\ UNCHANGED <<auth, gdoor, ext>>

\* Cloud bot guest B: reaches the Mac only through the cloud, like the app door.
\* Bound: B sends only to m2 (its row is on both Macs; the m1 slot stays idle) to keep runs short.
BotStart(m) ==
    /\ m \in BotMacs
    /\ turn[m]["B"] = "idle"
    /\ net[m] = "online"
    /\ RelayOK
    /\ GuardB(m)
    /\ turn' = [turn EXCEPT ![m]["B"] = "running"]
    /\ UNCHANGED <<auth, door, gdoor, ext>>

\* P writes on Telegram while m is offline: Telegram queues it.
TgQueue(m) ==
    /\ turn[m]["P"] = "idle"
    /\ net[m] = "offline"
    /\ turn' = [turn EXCEPT ![m]["P"] = "queued"]
    /\ door' = [door EXCEPT ![m] = "bridge"]
    /\ UNCHANGED <<auth, gdoor, ext>>

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
    /\ UNCHANGED <<auth, gdoor, ext>>

(* ---- a running turn ---- *)
\* Helper spawn re-checks may() (I2) and its door's gate. A refused spawn changes nothing.
Helper(m, w) ==
    /\ turn[m][w] = "running"
    /\ May(m, w)
    /\ HelperGate(m, w)
    /\ turn' = [turn EXCEPT ![m][w] = "helping"]
    /\ UNCHANGED <<auth, door, gdoor, ext>>

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
                           IF NoDeliverRecheck \/ (GuardMay(m, w) /\ RelayOK) THEN "delivered" ELSE "aborted"]
    /\ UNCHANGED <<auth, door, gdoor, ext>>

\* Optional: the door stops a live turn once the revoke reached m.
Stop(m, w) ==
    /\ turn[m][w] \in Live
    /\ revoked[m]
    /\ turn' = [turn EXCEPT ![m][w] = "aborted"]
    /\ UNCHANGED <<auth, door, gdoor, ext>>

(* ---- guest door (kernel/lib/kith_door.py, kith-auth door) ---- *)
\* A live turn of P asks for the door (the owner's job card, if any, is answered before this). kith_door.run takes
\* kith-auth's lock, reads `kith-auth door` (may(), not held, the job ticked), writes the claim and starts the box:
\* one step. Any door P's turn came through: the run is already going, the door is a tool call inside it.
DoorGate(m) == MayP(m) /\ (DoorNoHold \/ InSync(m))
DoorCheck(m) ==
    /\ DoorSplit
    /\ m \in DoorMacs
    /\ gdoor[m] = "idle"
    /\ turn[m]["P"] \in Live
    /\ DoorGate(m)
    /\ gdoor' = [gdoor EXCEPT ![m] = "checked"]
    /\ UNCHANGED <<auth, turn, door, ext>>
DoorStart(m) ==
    /\ IF DoorSplit
       THEN gdoor[m] = "checked"
       ELSE /\ m \in DoorMacs
            /\ gdoor[m] = "idle"
            /\ turn[m]["P"] \in Live
            /\ DoorGate(m)
    /\ gdoor' = [gdoor EXCEPT ![m] = "running"]
    /\ UNCHANGED <<auth, turn, door, ext>>
\* The box ends by itself (render done or failed).
DoorEnd(m) ==
    /\ gdoor[m] = "running"
    /\ gdoor' = [gdoor EXCEPT ![m] = "done"]
    /\ UNCHANGED <<auth, turn, door, ext>>
\* The sweep (cut_off on a revoke, the bot-guest sweep, the host's 60 s loop): under the lock, a claimed door whose
\* person may no longer run it is killed (held alone doesn't end one). A checked, unclaimed door is not seen.
DoorSweep(m) ==
    /\ ~NoDoorSweep
    /\ gdoor[m] = "running"
    /\ ~MayP(m)
    /\ gdoor' = [gdoor EXCEPT ![m] = "killed"]
    /\ UNCHANGED <<auth, turn, door, ext>>

(* ---- a bot's guest door (kith-auth door_state for a bot: owner_cut) ---- *)
\* B's own tick names the door; P's may not. door_state for B: may() (MayB, so a revoked P's bot is refused), not held
\* (in sync), then B's ticks cut to what P's run gets (owner_cut, the cut resolve_bot makes). P's bot is an invitee's:
\* Kai's own bots are not cut, and are not in this model.
BotDoorGrant(m) == MayB(m) /\ (BotDoorNoCut \/ ptick)
BotDoorGate(m) == BotDoorGrant(m) /\ InSync(m)
\* The owner gives P the job, or takes it back (kith-auth job video P make-videos [--rm]).
OwnerTick ==
    /\ BotDoor
    /\ ptick' = ~ptick
    /\ UNCHANGED <<auth, turn, door, gdoor, ext4, bdoor, alw, link>>
BotDoorStart(m) ==
    /\ BotDoor
    /\ m \in BotMacs
    /\ bdoor[m] = "idle"
    /\ turn[m]["B"] \in Live
    /\ BotDoorGate(m)
    /\ bdoor' = [bdoor EXCEPT ![m] = "running"]
    /\ UNCHANGED <<auth, turn, door, gdoor, ext4, ptick, alw, link>>
BotDoorEnd(m) ==
    /\ bdoor[m] = "running"
    /\ bdoor' = [bdoor EXCEPT ![m] = "done"]
    /\ UNCHANGED <<auth, turn, door, gdoor, ext4, ptick, alw, link>>
\* The same sweep: a claim whose `kith-auth door` is no longer ok (held excepted) is killed.
BotDoorSweep(m) ==
    /\ ~NoDoorSweep
    /\ bdoor[m] = "running"
    /\ ~BotDoorGrant(m)
    /\ bdoor' = [bdoor EXCEPT ![m] = "killed"]
    /\ UNCHANGED <<auth, turn, door, gdoor, ext4, ptick, alw, link>>

(* ---- delegations and the wakes their answers make (kith-ask, kith-wake, cards/resume.rs) ---- *)
\* A live turn of P starts a `kith ask`: its record names P's session as the caller.
AskStart(m) ==
    /\ Asks
    /\ m = "m1"   \* bound: P asks from m1 only, to keep the runs short
    /\ ask[m] = "none"
    /\ turn[m]["P"] \in Live
    /\ MayP(m)
    /\ ask' = [ask EXCEPT ![m] = "open"]
    /\ UNCHANGED <<auth, turn, door, gdoor, items, parked, gone, cardst, cardhit, wakeOwed, hrev, hat, clk, hops, bdoor, ptick, alw, link>>
\* The asked agent answers; nobody waits, so the answer is a wake of P's session. An ended ask keeps nothing (R8).
AskAnswer(m) ==
    /\ Asks
    /\ ask[m] = "open" \/ (AnswerIgnoresEnd /\ ask[m] = "stopped")
    /\ ask' = [ask EXCEPT ![m] = IF ask[m] = "open" THEN "answered" ELSE ask[m]]
    /\ wakeOwed' = [wakeOwed EXCEPT ![m] = TRUE]
    /\ UNCHANGED <<auth, turn, door, gdoor, items, parked, gone, cardst, cardhit, hrev, hat, clk, hops, bdoor, ptick, alw, link>>
\* The owed wake starts P's turn: the host's Recheck reads may().
WakeStart(m) ==
    /\ Asks
    /\ wakeOwed[m]
    /\ turn[m]["P"] \in {"idle", "delivered", "aborted"}
    /\ MayP(m)
    /\ turn' = [turn EXCEPT ![m]["P"] = "running"]
    /\ wakeOwed' = [wakeOwed EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<auth, door, gdoor, items, parked, gone, cardst, cardhit, ask, hrev, hat, clk, hops, bdoor, ptick, alw, link>>
\* The core's claim sweep (revoke_person, M3-10d): once the revoked fact is on m, P's live turns there, and B's unless
\* SkipBots, are stopped. A later step than the revoke itself (two transactions). For a removal and for a suspend this is the code
\* (people.suspend records the order in open_removals and runs it after the commit). The starts are already
\* refused by the fence, this stops the ones that began before it.
SweptWho == IF SkipBots THEN {"P"} ELSE Who
\* ClaimEnding: the core's commit. With KillSplit the process is still running, behind a claim that is `ending`.
\* A `reported` claim is in the revoke's kill list too; the revoke rewrites its reason to `revoked` (claims.rs revoke_person).
ClaimEnding(m) ==
    /\ CoreSweep
    /\ revoked[m]
    /\ \E w \in SweptWho : turn[m][w] \in Live \cup {"reported"}
    /\ turn' = [turn EXCEPT ![m] = [w \in Who |-> IF w \in SweptWho /\ turn[m][w] \in Live
                                                   THEN (IF KillSplit THEN "ending" ELSE "aborted")
                                                   ELSE IF w \in SweptWho /\ turn[m][w] = "reported"
                                                   THEN (IF RevokeKeepsReason THEN "rending" ELSE "ending")
                                                   ELSE turn[m][w]]]
    /\ UNCHANGED <<auth, door, gdoor, ext>>
\* KillSent: the host's one signal (end_kills -> end_claimed on a detached task) lands.
KillSent(m) ==
    /\ KillSplit
    /\ \E w \in Who : turn[m][w] \in {"ending", "rending"}
    /\ turn' = [turn EXCEPT ![m] = [w \in Who |-> IF turn[m][w] \in {"ending", "rending"} THEN "aborted" ELSE turn[m][w]]]
    /\ UNCHANGED <<auth, door, gdoor, ext>>
\* The host restarts, or the spawn fails, before the signal goes: nothing remembers the kill.
KillLost(m, w) ==
    /\ KillSplit /\ HostCrash
    /\ turn[m][w] \in {"ending", "rending"}
    /\ turn' = [turn EXCEPT ![m][w] = IF turn[m][w] = "ending" THEN "stuck" ELSE "rstuck"]
    /\ UNCHANGED <<auth, door, gdoor, ext>>
\* KillReSend: each supervisor pass signals every `ending` claim whose process positively matches its stamp (level-triggered:
\* it reads the claims table, not what the host remembers). A gone or reused pid is left alone, so it is not a model step.
KillReSend(m) ==
    /\ KillSplit /\ ReSendKills
    /\ ~ReSendNeedsLink \/ link[m]
    /\ \E w \in Who : turn[m][w] \in {"ending", "stuck"}
    /\ turn' = [turn EXCEPT ![m] = [w \in Who |-> IF turn[m][w] \in {"ending", "stuck"} THEN "aborted" ELSE turn[m][w]]]
    /\ UNCHANGED <<auth, door, gdoor, ext>>
\* A nameless done/exit: the claim leaves the live set for `ending` with that reason; the process is the claimer's own business.
Report(m, w) ==
    /\ NamelessReport
    /\ turn[m][w] \in Live
    /\ turn' = [turn EXCEPT ![m][w] = "reported"]
    /\ UNCHANGED <<auth, door, gdoor, ext>>
\* The holder link on m goes down or comes back (a remote holder, an absent one, a failing one).
LinkFlip(m) ==
    /\ LinkFlaps
    /\ link' = [link EXCEPT ![m] = ~link[m]]
    /\ UNCHANGED nolink
\* A revoke on m (cut_off -> sweep_asks): P's open delegation ends (kith ask --end) ...
\* With the core's order (revoke_order.rs) only after the claims are swept; AsksFirst drops that wait.
AskSweep(m) ==
    /\ Asks
    /\ ~NoAskSweep
    /\ revoked[m]
    /\ ~CoreSweep \/ AsksFirst \/ (\A w \in Who : turn[m][w] \notin Live)
    /\ ask[m] = "open"
    /\ ask' = [ask EXCEPT ![m] = "stopped"]
    /\ UNCHANGED <<auth, turn, door, gdoor, items, parked, gone, cardst, cardhit, wakeOwed, hrev, hat, clk, hops, bdoor, ptick, alw, link>>
\* ... and the wake owed to P's session is withdrawn (kith-wake withdraw-person).
WakeWithdraw(m) ==
    /\ Asks
    /\ ~NoWakeWithdraw
    /\ revoked[m]
    /\ wakeOwed[m]
    /\ wakeOwed' = [wakeOwed EXCEPT ![m] = FALSE]
    /\ UNCHANGED <<auth, turn, door, gdoor, items, parked, gone, cardst, cardhit, ask, hrev, hat, clk, hops, bdoor, ptick, alw, link>>

(* ---- a person known only by a chat handle (remote/handle_revokes.rs) ---- *)
\* The owner revokes or undoes Q on m's terminal or app: stamped, kept. With no catch-up the change is sent only now.
HChange(m) ==
    /\ HandleSync
    /\ hops < 2
    /\ clk' = clk + 1
    /\ hops' = hops + 1
    /\ LET v == ~hrev[m]
           \* A clock that runs behind reads low; the stamp is then lifted above the one kept (put_handle_revoke ignores
           \* a stamp that is not newer), unless HandleNoBump.
           st == IF HandleSkew /\ m = "m2" THEN (IF HandleNoBump THEN 0 ELSE hat[m] + 1) ELSE clk + 1
           kept == st > hat[m]
           toAll(n) == n = m \/ (HandleNoCatchup /\ net[m] = "online" /\ net[n] = "online")
       IN /\ hrev' = [n \in Macs |-> IF toAll(n) THEN v ELSE hrev[n]]
          /\ hat' = [n \in Macs |-> IF kept /\ toAll(n) THEN st ELSE hat[n]]
    /\ UNCHANGED <<auth, turn, door, gdoor, items, parked, gone, cardst, cardhit, ask, wakeOwed, bdoor, ptick, alw, link>>
\* Two paired Macs, both online, exchange their lists (peerRevokes): the latest stamp wins on both.
HExchange(a, b) ==
    /\ HandleSync
    /\ ~HandleNoCatchup
    /\ a # b
    /\ net[a] = "online" /\ net[b] = "online"
    /\ IF HandleNoStamp
       THEN /\ hrev' = [hrev EXCEPT ![b] = hrev[a]]
            /\ hat' = [hat EXCEPT ![b] = hat[a]]
       ELSE \* the newer stamp wins; an equal one changes nothing (the receiver knows it already)
            /\ hrev' = [n \in Macs |-> IF hat[a] > hat[b] THEN hrev[a] ELSE IF hat[b] > hat[a] THEN hrev[b] ELSE hrev[n]]
            /\ hat' = [n \in Macs |-> IF hat[a] > hat[b] THEN hat[a] ELSE hat[b]]
    /\ UNCHANGED <<auth, turn, door, gdoor, items, parked, gone, cardst, cardhit, ask, wakeOwed, clk, hops, bdoor, ptick, alw, link>>
\* The same exchange where applying a's newer change on b fails: with the stamp kept first, b then believes it has it.
HExchangeFails(a, b) ==
    /\ HandleSync
    /\ HandleApplyFails
    /\ ~HandleNoCatchup
    /\ a # b
    /\ net[a] = "online" /\ net[b] = "online"
    /\ hat[a] > hat[b]
    /\ hat' = IF HandleStoreFirst THEN [hat EXCEPT ![b] = hat[a]] ELSE hat
    /\ UNCHANGED <<auth, turn, door, gdoor, items, parked, gone, cardst, cardhit, ask, wakeOwed, hrev, clk, hops, bdoor, ptick, alw, link>>
\* A peer sends a stamp from the far future; only a computer that does not bound stamps takes it.
HForge(m) ==
    /\ HandleSync
    /\ HandleFutureOk
    /\ net[m] = "online"
    /\ hat[m] < 9
    /\ hrev' = [hrev EXCEPT ![m] = TRUE]
    /\ hat' = [hat EXCEPT ![m] = 9]
    /\ UNCHANGED <<auth, turn, door, gdoor, items, parked, gone, cardst, cardhit, ask, wakeOwed, clk, hops, bdoor, ptick, alw, link>>

(* ---- removing a revoked person ---- *)
\* kith-auth `--rm` (or the owner dropping them) while P is revoked: what the revoke parked goes with them, on whichever
\* path (members.rs forget_removed), or it is orphaned and an undo would hand it to nobody.
Remove(m) ==
    /\ Removal
    /\ revoked[m]
    /\ ~gone[m]
    /\ gone' = [gone EXCEPT ![m] = TRUE]
    /\ parked' = [parked EXCEPT ![m] = IF RemoveKeeps THEN parked[m] ELSE {}]
    /\ cardst' = CardsAfter(m, TRUE)
    /\ cardhit' = CardHitAfter(m, TRUE)
    \* forget (write.rs forget): the grants made for agent@P go with the person
    /\ alw' = IF ForgetKeepsGrant THEN alw ELSE [alw EXCEPT ![m].gr = FALSE]
    /\ UNCHANGED <<auth, turn, door, gdoor, items, ask, wakeOwed, hrev, hat, clk, hops, bdoor, ptick, link>>

(* ---- P's card and its once grant (core/runtime/src/cards) ---- *)
\* A live turn of P asks the owner (bound: m1, once, before any revoke, so every card is a pre-revoke one).
CardAsk(m) ==
    /\ CardsOn
    /\ m = "m1"
    /\ revokes = 0
    /\ cardst[m] = "none"
    /\ turn[m]["P"] \in Live
    /\ cardst' = [cardst EXCEPT ![m] = "open"]
    /\ UNCHANGED <<auth, turn, door, gdoor, items, parked, gone, cardhit, ask, wakeOwed, hrev, hat, clk, hops, bdoor, ptick, alw, link>>
\* The owner's yes (cards::answer): only an open card takes one; it becomes a once grant.
CardYes(m) ==
    /\ CardsOn
    /\ cardst[m] = "open"
    /\ cardst' = [cardst EXCEPT ![m] = "approved"]
    /\ UNCHANGED <<auth, turn, door, gdoor, items, parked, gone, cardhit, ask, wakeOwed, hrev, hat, clk, hops, bdoor, ptick, alw, link>>
\* The session asks again: the unused grant is handed to it and spent (cards::raise, `Granted`). It reads no access.
CardPickup(m) ==
    /\ CardsOn
    /\ cardst[m] = "approved"
    /\ cardst' = [cardst EXCEPT ![m] = "used"]
    /\ UNCHANGED <<auth, turn, door, gdoor, items, parked, gone, cardhit, ask, wakeOwed, hrev, hat, clk, hops, bdoor, ptick, alw, link>>

(* ---- the always of an escape card (C-a): kith-auth access --by, answered by the core on the tombstone ---- *)
\* Written on m1 only (the other Mac would repeat it).
AlwaysMacs == IF AlwaysOn THEN {"m1"} ELSE {}
AlwaysBy == {"owner", "self"}

\* The refusals of the one locked step: may() now (the core's own fact when routed: a revoked person is refused before any
\* row), a bot only within what its owner holds (the owner's entry is there), a self-approval only while P is a member.
AlwaysMayNow(m, w) == IF RouterDoor THEN GuardMay(m, w) ELSE May(m, w)
AlwaysOK(m, w, by) ==
    /\ AlwaysNoMay \/ AlwaysMayNow(m, w)
    /\ w = "P" \/ BotIgnoresOwner \/ alw[m].e["P"] # "none"
    /\ by = "owner" \/ alw[m].mem

\* The owner's yes outlasts a person's; a person's never replaces the owner's.
AlwaysSet(m, w, by) ==
    alw' = [alw EXCEPT ![m].e[w] = IF alw[m].e[w] = "owner" THEN "owner" ELSE by,
                       ![m].chk[w] = "none",
                       ![m].bad[w] = alw[m].bad[w] \/ ~May(m, w)]

AlwaysWrite(m, w) ==
    /\ AlwaysOn
    /\ ~AlwaysSplit
    /\ m \in AlwaysMacs
    /\ \E by \in AlwaysBy : AlwaysOK(m, w, by) /\ AlwaysSet(m, w, by)
    /\ UNCHANGED <<auth, turn, door, gdoor, ext4, bdoor, ptick, link>>

\* The broken split: the check reads may() now, the write comes later.
AlwaysCheck(m, w) ==
    /\ AlwaysSplit
    /\ m \in AlwaysMacs
    /\ alw[m].chk[w] = "none"
    /\ \E by \in AlwaysBy : AlwaysOK(m, w, by) /\ alw' = [alw EXCEPT ![m].chk[w] = by]
    /\ UNCHANGED <<auth, turn, door, gdoor, ext4, bdoor, ptick, link>>
AlwaysCommit(m, w) ==
    /\ AlwaysSplit
    /\ m \in AlwaysMacs
    /\ alw[m].chk[w] # "none"
    /\ AlwaysSet(m, w, alw[m].chk[w])
    /\ UNCHANGED <<auth, turn, door, gdoor, ext4, bdoor, ptick, link>>

\* A removal takes the entry as it is written, from anyone, a revoked P included: no check stands in the way.
AlwaysRemove(m, w) ==
    /\ AlwaysOn
    /\ m \in AlwaysMacs
    /\ alw[m].e[w] # "none"
    /\ alw' = [alw EXCEPT ![m].e[w] = "none", ![m].bad[w] = FALSE]
    /\ UNCHANGED <<auth, turn, door, gdoor, ext4, bdoor, ptick, link>>

(* ---- the secret grant for P's own account (S-b): kith-auth grant AGENT@P SERVICE PROFILE, the core's people.secretGrant ---- *)
\* need_person(P) is read in the same locked step that writes the grant, so it cannot land for an id that was just removed
\* (a removed id taken again by someone else would otherwise inherit the profile). grbad is the oracle (no step reads it).
GrantMacs == IF GrantOn THEN {"m1"} ELSE {}
GrantWrite(m) ==
    /\ GrantOn
    /\ m \in GrantMacs
    /\ GrantNoPerson \/ ~gone[m]
    /\ alw' = [alw EXCEPT ![m].gr = TRUE, ![m].grbad = (alw[m].grbad \/ gone[m])]
    /\ UNCHANGED <<auth, turn, door, gdoor, ext4, bdoor, ptick, link>>

\* P is made a guest (or leaves the agent): the entries P approved go in the same step (write.rs drop_self_approved, M3).
Demote(m) ==
    /\ AlwaysOn
    /\ m \in AlwaysMacs
    /\ alw[m].mem
    /\ alw' = [alw EXCEPT ![m].mem = FALSE,
                          ![m].e = IF DemoteKeeps THEN alw[m].e
                                   ELSE [w \in Who |-> IF alw[m].e[w] = "self" THEN "none" ELSE alw[m].e[w]]]
    /\ UNCHANGED <<auth, turn, door, gdoor, ext4, bdoor, ptick, link>>

\* Host restart (round 1's Restart) is left out here: it only aborts live turns, which no property depends on.

Next ==
    \/ AppRevoke
    \/ AppUndo
    \/ OwnerTick
    \/ \E m \in Macs :
          \/ CliRevoke(m)
          \/ CliUndo(m)
          \/ Push(m)
          \/ CloudDeliver(m)
          \/ CoreMirror(m)
          \/ CoreSide(m)
          \/ GateAsk(m)
          \/ \E n \in Nets \ {net[m]} : SetNet(m, n)
          \/ AppStart(m)
          \/ BotStart(m)
          \/ TgQueue(m)
          \/ BridgeRecv(m)
          \/ DoorCheck(m)
          \/ DoorStart(m)
          \/ DoorEnd(m)
          \/ DoorSweep(m)
          \/ BotDoorStart(m)
          \/ BotDoorEnd(m)
          \/ BotDoorSweep(m)
          \/ CardAsk(m)
          \/ CardYes(m)
          \/ CardPickup(m)
          \/ AskStart(m)
          \/ AskAnswer(m)
          \/ WakeStart(m)
          \/ AskSweep(m)
          \/ ClaimEnding(m)
          \/ KillSent(m)
          \/ KillReSend(m)
          \/ LinkFlip(m)
          \/ WakeWithdraw(m)
          \/ HChange(m)
          \/ HForge(m)
          \/ Remove(m)
          \/ Demote(m)
          \/ GrantWrite(m)
    \/ \E a, b \in Macs : HExchange(a, b) \/ HExchangeFails(a, b)
    \/ \E m \in Macs, w \in Who :
          \/ Helper(m, w)
          \/ Deliver(m, w)
          \/ Stop(m, w)
          \/ KillLost(m, w)
          \/ Report(m, w)
          \/ AlwaysWrite(m, w)
          \/ AlwaysCheck(m, w)
          \/ AlwaysCommit(m, w)
          \/ AlwaysRemove(m, w)

\* The sweep loop runs every 60 s: weak fairness (only DoorD1, a liveness property, depends on it).
Spec == Init /\ [][Next]_vars
        /\ (\A m \in Macs : WF_vars(DoorSweep(m)))
        /\ (\A m \in Macs : WF_vars(AskSweep(m)) /\ WF_vars(WakeWithdraw(m)) /\ WF_vars(ClaimEnding(m)) /\ WF_vars(KillSent(m)) /\ WF_vars(KillReSend(m)))
        /\ WF_vars(SetNet("m2", "online")) /\ WF_vars(\E a, b \in Macs : HExchange(a, b))

(* ---- properties ---- *)
TypeOK ==
    /\ rows \in [Macs -> SUBSET Kinds]
    /\ revoked \in [Macs -> BOOLEAN]
    /\ crev \in [Macs -> [v : BOOLEAN, side : BOOLEAN]]
    /\ ver \in [Macs -> 0..2]
    /\ outbox \in [Macs -> BOOLEAN]
    /\ net \in [Macs -> Nets]
    /\ cloudRevoked \in BOOLEAN
    /\ cloudVer \in 0..2
    /\ revokes \in 0..1
    /\ undos \in 0..1
    /\ turn \in [Macs -> [Who -> Status]]
    /\ door \in [Macs -> Doors]
    /\ gdoor \in [Macs -> GDoor]
    /\ \A m \in Macs : ver[m] <= cloudVer
    /\ items \in [Macs -> SUBSET Items0]
    /\ parked \in [Macs -> SUBSET Items0]
    /\ gone \in [Macs -> BOOLEAN]
    /\ ask \in [Macs -> AskState]
    /\ wakeOwed \in [Macs -> BOOLEAN]
    /\ hrev \in [Macs -> BOOLEAN]
    /\ hat \in [Macs -> 0..9]
    /\ clk \in 0..2
    /\ hops \in 0..2
    /\ bdoor \in [Macs -> GDoor]
    /\ ptick \in BOOLEAN
    /\ link \in [Macs -> BOOLEAN]
    /\ cardst \in [Macs -> CardSt]
    /\ cardhit \in [Macs -> BOOLEAN]
    /\ alw \in [Macs -> [mem : BOOLEAN, e : [Who -> AlwaysBy \cup {"none"}], chk : [Who -> AlwaysBy \cup {"none"}],
                         bad : [Who -> BOOLEAN], gr : BOOLEAN, grbad : BOOLEAN]]

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
DoorStartOf(m) == gdoor[m] # "running" /\ gdoor'[m] = "running"
\* DoorD2 (GuestDoor.tla D2): once the revoke reached a Mac, no guest door of P starts there ...
DoorD2 == [][\A m \in Macs : DoorStartOf(m) => ~revoked[m]]_vars
\* ... and once the cloud has it, none starts on any Mac (the held check: a Mac behind the cloud waits).
DoorD2c == [][\A m \in Macs : DoorStartOf(m) => ~cloudRevoked]_vars
\* DoorD1 (GuestDoor.tla D1): a door running for a revoked P is stopped (or the revoke is undone).
DoorD1 == \A m \in Macs : (gdoor[m] = "running" /\ revoked[m]) ~> (gdoor[m] # "running" \/ ~revoked[m])
\* BotDoorCut (I6 for doors): a guest door of B starts only while its owner P holds it.
\* CoreDoorHook (M2-14), at the level of the door TABLE: no moment where P is revoked on a Mac and a row of a door of P or B
\* is still open there, with no fairness needed. It says nothing about the box (see DoorsSwept). Fails without the hook
\* (the sweep is a later action: the window the host's sweep leaves).
DoorsCutWithRevoke == \A m \in Macs : revoked[m] => (gdoor[m] # "running" /\ bdoor[m] # "running")
\* A swept row is final: undo (or anything) never brings it back (the core's swept rows stay swept). Fails in
\* broken-door-undo-revives.
DoorNoResurrect == [][\A m \in Macs : (gdoor[m] = "killed" => gdoor'[m] = "killed") /\ (bdoor[m] = "killed" => bdoor'[m] = "killed")]_vars
BotDoorCut == [][\A m \in Macs : (bdoor[m] # "running" /\ bdoor'[m] = "running") => ptick]_vars

\* ItemsCut: a revoked P's devices, memberships and open invites grant nothing at once.
ItemsCut == \A m \in Macs : revoked[m] => items[m] = {}
\* ItemsExact: wherever P is not revoked, they are what they were (undo restores exactly; a removal is another path).
ItemsExact == \A m \in Macs : ~revoked[m] => items[m] = Items0
\* StoppedNeverWakes: an ended ask keeps no answer and wakes no one, whoever answers when (R8).
StoppedNeverWakes == [][\A m \in Macs : (ask[m] = "stopped" /\ ~wakeOwed[m]) => ~wakeOwed'[m]]_vars
\* AsksEnded: a delegation of a revoked P is ended (or the revoke is undone).
AsksEnded == \A m \in Macs : (revoked[m] /\ ask[m] = "open") ~> (ask[m] # "open" \/ ~revoked[m])
\* OrderKept (M3-10d): a delegation of a revoked P is ended only once P's and B's live turns on m are stopped. Fails when
\* the asks go first.
OrderKept == \A m \in Macs : (revoked[m] /\ ask[m] = "stopped") => (\A w \in Who : turn[m][w] \notin Live)
\* Swept (M3-10d): a live turn of a revoked P or B is stopped (or the revoke is undone). Fails when the sweep skips B.
Swept == \A m \in Macs, w \in Who : (revoked[m] /\ turn[m][w] \in Live) ~> (turn[m][w] \notin Live \/ ~revoked[m])
\* NoLiveProc (M2-23 gate): a revoked person's turn or claim process, live or behind an `ending` claim, is gone (or the revoke is
\* undone). Needs KillReSend when the host can lose its one kill (HostCrash).
NoLiveProc == \A m \in Macs, w \in Who : (revoked[m] /\ turn[m][w] \in Proc) ~> (turn[m][w] \notin Proc \/ ~revoked[m])
\* WakesWithdrawn: the wake owed to a revoked P is dropped (or the revoke is undone).
WakesWithdrawn == \A m \in Macs : (revoked[m] /\ wakeOwed[m]) ~> (~wakeOwed[m] \/ ~revoked[m])
\* HandleConverges: the Macs end up agreeing on Q, the Mac that was off included.
HandleConverges == <>[](hrev["m1"] = hrev["m2"] /\ hat["m1"] = hat["m2"])
\* HandleStampsGrow: a Mac never takes an older fact over a newer one.
HandleStampsGrow == [][\A m \in Macs : hat'[m] >= hat[m]]_vars
\* HandleStampsMove: a Mac's answer about Q changes only with a newer stamp kept beside it (a change whose stamp is
\* ignored leaves the other Macs deaf to it).
HandleStampsMove == [][\A m \in Macs : hrev'[m] # hrev[m] => hat'[m] > hat[m]]_vars
\* HandleBounded: no stamp is ahead of every real change (a peer's far-future stamp would win over all after it).
HandleBounded == \A m \in Macs : hat[m] <= clk
\* NoOrphan: a removed P leaves nothing parked.
NoOrphan == \A m \in Macs : gone[m] => parked[m] = {}
\* NoGrantForGone (S-b): a grant is never written for a person who is gone, and removing a person takes their grants.
\* Fails in broken-grant-no-person and broken-forget-keeps-grant.
NoGrantForGone == \A m \in Macs : ~alw[m].grbad /\ (gone[m] => ~alw[m].gr)
\* A grant for P is written (probe: reachable).
NeverGrantWritten == \A m \in Macs : ~alw[m].gr
\* HandleNoCloud: a handle revoke changes nothing in the cloud.
HandleNoCloud == [][(hrev' # hrev) => (cloudRevoked' = cloudRevoked /\ cloudVer' = cloudVer)]_vars

\* CardsCut (M2-11b): a yes given before a revoke landed on m is never handed to the session afterwards, undo or no undo;
\* and an answer to a card a revoke closed changes nothing (CardYes needs it open). Fails in broken-card-no-withdraw
\* and broken-card-undo-revives.
CardsCut == [][\A m \in Macs : (cardst[m] = "approved" /\ cardst'[m] = "used") => ~cardhit[m]]_vars
\* A withdrawn card is final. Fails in broken-card-undo-revives.
CardNoResurrect == [][\A m \in Macs : cardst[m] = "withdrawn" => cardst'[m] = "withdrawn"]_vars

\* AlwaysOnlyIfMay (C-a): no always was written while may() was false (R4 for the new rows of a person: an entry written while
\* revoked would come back with the undo). Fails in broken-always-no-may and broken-always-split.
AlwaysOnlyIfMay == \A m \in Macs, w \in Who : ~alw[m].bad[w]
\* AlwaysBotBelowOwner: a bot's entry counts only with its owner's (owner_cut); fails when the bot ignores its owner.
AlwaysBotBelowOwner == \A m \in Macs :
    (alw[m].e["B"] # "none" /\ (BotIgnoresOwner \/ alw[m].e["P"] # "none")) => alw[m].e["P"] # "none"
\* ApproverDemoted (M3): an entry a member approved for themselves stays only while they are a member. Fails in broken-demote-keeps.
ApproverDemoted == \A m \in Macs, w \in Who : alw[m].e[w] = "self" => alw[m].mem

(* ---- probes: each must FAIL on the design (a reachability witness), unless noted ---- *)
\* The core takes the revoked fact (the mirror is reachable) ...
NeverCoreRevoked == \A m \in Macs : ~crev[m].v
\* ... and there is a moment when the core has not been told, which is when the gate is shut and the host asks kith-auth.
NeverGateShut == \A m \in Macs : crev[m].v = revoked[m]
\* Switched: the core's copy and the file's fact are never apart (there is one). Fails under SwitchLag and CoreOnlyWrite.
CoreInStep == NeverGateShut   \* the same fact; this name is the one a switched row asserts
\* The latch is open on an out-of-step core only after a core-only write, and never switched. Run on switched and core-gate.
SideOnlyLatched == \A m \in Macs : crev[m].side => (CoreOnlyWrite /\ ~Switched)
\* A card before any revoke is picked up (the card path is reachable).
NeverCardUsed == \A m \in Macs : cardst[m] # "used"
\* ... and a revoke does withdraw one.
NeverCardWithdrawn == \A m \in Macs : cardst[m] # "withdrawn"
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
\* A member's door does run ...
NeverDoorRuns == \A m \in Macs : gdoor[m] # "running"
\* ... and the sweep does stop one (P revoked while the door ran).
NeverDoorKilled == \A m \in Macs : gdoor[m] # "killed"
\* B's door does run (P given the job) ...
NeverBotDoorRuns == \A m \in Macs : bdoor[m] # "running"
\* ... and the sweep stops it once the owner takes P's job back.
NeverBotDoorKilled == \A m \in Macs : bdoor[m] # "killed"
\* Probes of round 4: a parked item does come back; an ask is answered into a wake; Q is revoked on the second Mac.
NeverItemsBack == ~(undos = 1 /\ \E m \in Macs : parked[m] = {} /\ items[m] = Items0 /\ ~revoked[m] /\ ver[m] = 2)
NeverAnswerWake == \A m \in Macs : ~wakeOwed[m]
NeverHandleOnPeer == ~hrev["m2"]
\* An always is written (the card path is reachable) ...
NeverAlwaysWritten == \A m \in Macs, w \in Who : alw[m].e[w] = "none"
\* ... one P approved for themselves, and the demotion that drops it.
NeverSelfAlways == \A m \in Macs, w \in Who : alw[m].e[w] # "self"
NeverMemberLost == \A m \in Macs : alw[m].mem
\* The bridge never starts P's turn while m is cloudDown and nothing is revoked.
\* PASSES on the design (in-sync needs the cloud link: the cost, proved); FAILS on BridgeNoSync (reachable).
NeverBridgeInCloudDown ==
    [][\A m \in Macs : ~(StartOf(m, "P") /\ door'[m] = "bridge" /\ net[m] = "cloudDown" /\ revokes = 0)]_vars
====
