------------------------------ MODULE Secrets ------------------------------
(***************************************************************************)
(* One secret card (key card) and the order of its steps: two owner        *)
(* devices answering, the clock expiring it, a no, a write that fails.     *)
(*                                                                         *)
(* Code: serve/host/src/cards/secret.rs (provide, respond, deny, grant),   *)
(* serve/host/src/store/cards.rs (card_claim, card_claim_release,          *)
(* card_saved, card_answer, cards_sweep); and the core's version,          *)
(* core/runtime/src/cards/{mod,secret}.rs (claim, release, saved,          *)
(* withdraw, sweep, park_run, provide) over secrets/ (the envelope).       *)
(*                                                                         *)
(*   Claim    = Slot::take + card_claim: one device, card open and before  *)
(*              its deadline; the deadline is pushed out for the write.    *)
(*   Write    = `kith-auth set ...` (the Keychain changes).                *)
(*   Finish   = card_saved: the card is answered, after the write.         *)
(*   Fail     = the write failed: claim released, card still open.         *)
(*   Sweep    = cards_sweep: an open card past its deadline expires.       *)
(*   Deny     = respondPermission deny: refused while a slot is held.      *)
(*                                                                         *)
(*   Withdraw = withdraw: the asker cancelled; waits for a held claim       *)
(*              (WithdrawWaits: the core's withdraw, and the host's since   *)
(*              M2-12: cards::secret::claimed).                             *)
(*   Crash    = the writer's process died; its claim stays until it is     *)
(*              dropped (Lapse). Only with CrashPossible: the host's       *)
(*              restarts are not modeled, the core's daemon can die.       *)
(*   Lapse    = a claim ends without its holder: a dead pid is dropped,    *)
(*              or a pid-less claim's time runs out. With a live writer    *)
(*              the core refuses it (LapseNeedsDeadWriter): a claim with   *)
(*              a pid holds past its time.                                 *)
(*   Commit   = the core's one transaction: the claim is still ours, the   *)
(*              card is open, the key is sealed and the card is saved,     *)
(*              all or nothing (AtomicCommit). The host's Write + Finish   *)
(*              are two steps (AtomicCommit FALSE), so a crash between    *)
(*              them leaves a written, open card: see core-split.          *)
(*   Park (park_run) keeps the card open and its claim: no action here.    *)
(*                                                                         *)
(* Two switches show the checks are not vacuous: Exclusive FALSE lets a    *)
(* second device claim too; ClaimBlocksClose FALSE lets the sweep and a    *)
(* no close a card whose key is being written.                             *)
(*                                                                         *)
(* A third, FailWritesNothing, is an assumption, not a guard: the model    *)
(* takes a failed write to have written nothing. FALSE lets a write that   *)
(* the host counts as failed (kith-auth exits non-zero after a Keychain    *)
(* item went in, or runs past TOOL_TIMEOUT and is killed after writing)    *)
(* still have written; the rows for it show what that assumption hides.    *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS Exclusive, ClaimBlocksClose, FailWritesNothing, AtomicCommit, CrashPossible, LapseNeedsDeadWriter, WithdrawWaits,
          Switched, CoreFirst, LostAsFail, ClaimGap

Devices == {"d1", "d2"}

VARIABLES
    card,     \* "open" "saved" "denied" "expired" "withdrawn"
    late,     \* the card's own deadline has passed (the clock only moves forward)
    claim,    \* the device holding the write claim, or "none"
    pc,       \* per device: "idle" "writing" "wrote" "done" "dead" (Switched: also "kcdone" "coredone")
    writes,   \* how many times the Keychain was written for this card (Switched: how many times the core recorded the key)
    wroteOpen, \* every write happened while the card was open
    kc,       \* Switched: the Keychain item exists
    ref       \* Switched: the core holds the reference (and so the grant) for it

vars == <<card, late, claim, pc, writes, wroteOpen, kc, ref>>

Init ==
    /\ card = "open"
    /\ late = FALSE
    /\ claim = "none"
    /\ pc = [d \in Devices |-> "idle"]
    /\ writes = 0
    /\ wroteOpen = TRUE
    /\ kc = FALSE
    /\ ref = FALSE

Tick == /\ ~late /\ late' = TRUE /\ UNCHANGED <<card, claim, pc, writes, wroteOpen, kc, ref>>

\* The slot and the store's claim, together: the card is open and before its deadline.
Claim(d) ==
    /\ pc[d] = "idle"
    /\ card = "open" /\ ~late
    /\ (Exclusive => claim = "none")
    /\ claim' = d
    /\ pc' = [pc EXCEPT ![d] = "writing"]
    /\ UNCHANGED <<card, late, writes, wroteOpen, kc, ref>>

Write(d) ==
    /\ ~AtomicCommit
    /\ ~Switched
    /\ pc[d] = "writing"
    /\ writes' = writes + 1
    /\ wroteOpen' = (wroteOpen /\ card = "open")
    /\ pc' = [pc EXCEPT ![d] = "wrote"]
    /\ UNCHANGED <<card, late, claim, kc, ref>>

Fail(d) ==
    /\ pc[d] = "writing"
    /\ pc' = [pc EXCEPT ![d] = "idle"]
    /\ claim' = IF claim = d THEN "none" ELSE claim
    /\ UNCHANGED <<card, late, writes, wroteOpen, kc, ref>>

\* Only when FailWritesNothing is FALSE: the Keychain changed, but the host was told the write failed (released, open).
\* Bounded (writes < 2) so the retries stay finite.
FailAfterWrite(d) ==
    /\ ~FailWritesNothing
    /\ pc[d] = "writing"
    /\ writes < 2
    /\ writes' = writes + 1
    /\ wroteOpen' = (wroteOpen /\ card = "open")
    /\ pc' = [pc EXCEPT ![d] = "idle"]
    /\ claim' = IF claim = d THEN "none" ELSE claim
    /\ UNCHANGED <<card, late, kc, ref>>

\* card_saved only answers a card that is still open.
Finish(d) ==
    /\ ~AtomicCommit
    /\ pc[d] = "wrote"
    /\ card = "open"
    /\ card' = "saved"
    /\ pc' = [pc EXCEPT ![d] = "done"]
    /\ claim' = IF claim = d THEN "none" ELSE claim
    /\ UNCHANGED <<late, writes, wroteOpen, kc, ref>>

\* The core's commit: one transaction. The claim is ours and the card open (saved_in checks both), the key is sealed
\* and the card is saved together, or nothing happens.
Commit(d) ==
    /\ AtomicCommit
    /\ pc[d] = "writing"
    /\ claim = d
    /\ card = "open"
    /\ writes' = writes + 1
    /\ wroteOpen' = (wroteOpen /\ card = "open")
    /\ card' = "saved"
    /\ pc' = [pc EXCEPT ![d] = "done"]
    /\ claim' = "none"
    /\ UNCHANGED <<late, kc, ref>>

\* --- Switched: `kith-auth set` on the authority tombstone (S-b). The Keychain item first, read back; then ONE core transaction
\* records the reference (and the grant). The claim the host holds covers both steps. The core gets names only.
\*
\*   KcWrite    = the Keychain item is written (and read back).
\*   CoreCommit = the core takes the reference; the host then saves the card (Finish).
\*   CoreFail   = the core refused, or no daemon answers: kith-auth exits non-zero, the host releases the claim and the
\*                card stays open. The item stays (an orphan, never referenced); a retry reuses it.
\*   CoreLost   = the core's answer is lost; kith-auth reads the core's tables once. If the reference is there it reports
\*                success (LostAsFail FALSE); with no read-back (LostAsFail) a core that took it is told as a failure.
\* CoreFirst = the broken order (reference, then Keychain): a crash or a refusal between leaves a grant with no item.
\* ClaimGap  = the broken claim: released between the two steps, so a second device writes too.
KcWrite(d) ==
    /\ Switched /\ ~CoreFirst
    /\ pc[d] = "writing"
    /\ kc' = TRUE
    /\ wroteOpen' = (wroteOpen /\ card = "open")
    /\ pc' = [pc EXCEPT ![d] = "kcdone"]
    /\ UNCHANGED <<card, late, claim, writes, ref>>

CoreCommit(d) ==
    /\ Switched /\ ~CoreFirst
    /\ pc[d] = "kcdone"
    /\ ref' = TRUE
    /\ writes' = writes + 1
    /\ wroteOpen' = (wroteOpen /\ card = "open")
    /\ pc' = [pc EXCEPT ![d] = "wrote"]
    /\ UNCHANGED <<card, late, claim, kc>>

CoreFail(d) ==
    /\ Switched /\ ~CoreFirst
    /\ pc[d] = "kcdone"
    /\ pc' = [pc EXCEPT ![d] = "idle"]
    /\ claim' = IF claim = d THEN "none" ELSE claim
    /\ UNCHANGED <<card, late, writes, wroteOpen, kc, ref>>

\* The answer is lost and the core took it: the read-back sees the reference (success), or, with no read-back, the host is told "failed".
CoreLostApplied(d) ==
    /\ Switched /\ ~CoreFirst
    /\ pc[d] = "kcdone"
    /\ ref' = TRUE
    /\ writes' = writes + 1
    /\ wroteOpen' = (wroteOpen /\ card = "open")
    /\ pc' = [pc EXCEPT ![d] = IF LostAsFail THEN "idle" ELSE "wrote"]
    /\ claim' = IF LostAsFail /\ claim = d THEN "none" ELSE claim
    /\ UNCHANGED <<card, late, kc>>

\* The answer is lost and the core never took it: the read-back finds nothing, kith-auth exits 5, the host fails the write.
CoreLostNotApplied(d) == CoreFail(d)

\* ClaimGap: the host lets go of the claim between the Keychain and the core.
GapRelease(d) ==
    /\ Switched /\ ClaimGap
    /\ pc[d] = "kcdone"
    /\ claim = d
    /\ claim' = "none"
    /\ UNCHANGED <<card, late, pc, writes, wroteOpen, kc, ref>>

\* CoreFirst: the core records the reference, then the Keychain is written.
CoreRefFirst(d) ==
    /\ Switched /\ CoreFirst
    /\ pc[d] = "writing"
    /\ ref' = TRUE
    /\ writes' = writes + 1
    /\ wroteOpen' = (wroteOpen /\ card = "open")
    /\ pc' = [pc EXCEPT ![d] = "coredone"]
    /\ UNCHANGED <<card, late, claim, kc>>

KcAfterRef(d) ==
    /\ Switched /\ CoreFirst
    /\ pc[d] = "coredone"
    /\ kc' = TRUE
    /\ pc' = [pc EXCEPT ![d] = "wrote"]
    /\ UNCHANGED <<card, late, claim, writes, wroteOpen, ref>>

KcRefusesAfterRef(d) ==
    /\ Switched /\ CoreFirst
    /\ pc[d] = "coredone"
    /\ pc' = [pc EXCEPT ![d] = "idle"]
    /\ claim' = IF claim = d THEN "none" ELSE claim
    /\ UNCHANGED <<card, late, writes, wroteOpen, kc, ref>>

\* The writer's process dies: mid-write, or (non-atomic) after the key went in and before the card was saved.
Crash(d) ==
    /\ CrashPossible
    \* Switched: the kith-auth process lives from the claim to its exit; the host's save (after "wrote") is not modeled
    /\ IF Switched THEN pc[d] \in {"writing", "kcdone", "coredone"} ELSE pc[d] \in {"writing", "wrote"}
    /\ pc' = [pc EXCEPT ![d] = "dead"]
    /\ UNCHANGED <<card, late, claim, writes, wroteOpen, kc, ref>>

\* A claim ends without its holder. The core drops a claim whose pid is dead (void_dead_claims) and, with no pid, at
\* its time; a claim whose writer lives is never lapsed (LapseNeedsDeadWriter).
Lapse ==
    /\ claim # "none"
    /\ (LapseNeedsDeadWriter => pc[claim] = "dead")
    /\ claim' = "none"
    /\ UNCHANGED <<card, late, pc, writes, wroteOpen, kc, ref>>

Held == claim # "none"

Sweep ==
    /\ card = "open" /\ late
    /\ (ClaimBlocksClose => ~Held)
    /\ card' = "expired"
    /\ UNCHANGED <<late, claim, pc, writes, wroteOpen, kc, ref>>

Withdraw ==
    /\ card = "open"
    /\ (WithdrawWaits => ~Held)
    /\ card' = "withdrawn"
    /\ UNCHANGED <<late, claim, pc, writes, wroteOpen, kc, ref>>

Deny ==
    /\ card = "open"
    /\ (ClaimBlocksClose => ~Held)
    /\ card' = "denied"
    /\ UNCHANGED <<late, claim, pc, writes, wroteOpen, kc, ref>>

Next ==
    \/ Tick \/ Sweep \/ Deny \/ Withdraw \/ Lapse
    \/ \E d \in Devices : Claim(d) \/ Write(d) \/ Fail(d) \/ FailAfterWrite(d) \/ Finish(d) \/ Commit(d) \/ Crash(d)
        \/ KcWrite(d) \/ CoreCommit(d) \/ CoreFail(d) \/ CoreLostApplied(d) \/ GapRelease(d)
        \/ CoreRefFirst(d) \/ KcAfterRef(d) \/ KcRefusesAfterRef(d)

Spec == Init /\ [][Next]_vars

TypeOK ==
    /\ card \in {"open", "saved", "denied", "expired", "withdrawn"}
    /\ writes \in 0..4
    /\ kc \in BOOLEAN /\ ref \in BOOLEAN

\* Keychain written => the card was open when it was written ...
WriteWhileOpen == writes > 0 => wroteOpen

\* ... and a closed (denied, expired or withdrawn) card was never written.
ClosedNeverWritten == card \in {"denied", "expired", "withdrawn"} => writes = 0

\* ... and at most one write per card.
AtMostOneWrite == writes <= 1

\* ... and it ends saved: once nothing is in flight, a written card is saved.
WrittenEndsSaved ==
    (writes = 1 /\ (\A d \in Devices : pc[d] \notin {"writing", "wrote", "kcdone", "coredone"})) => card = "saved"

\* A failed write changes nothing the card shows: it stays open and unwritten by that step.
FailLeavesOpen == [][(\E d \in Devices : Fail(d) \/ CoreFail(d)) => (card' = card /\ writes' = writes)]_vars

\* Switched: the core never holds a reference (so never a grant) for a Keychain item that is not there. A crash between the
\* two steps leaves an orphan item, which nothing reads; the other order would leave a grant with no key.
GrantHasItem == ref => kc

=============================================================================
