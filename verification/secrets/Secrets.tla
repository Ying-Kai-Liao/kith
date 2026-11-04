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

CONSTANTS Exclusive, ClaimBlocksClose, FailWritesNothing, AtomicCommit, CrashPossible, LapseNeedsDeadWriter, WithdrawWaits

Devices == {"d1", "d2"}

VARIABLES
    card,     \* "open" "saved" "denied" "expired" "withdrawn"
    late,     \* the card's own deadline has passed (the clock only moves forward)
    claim,    \* the device holding the write claim, or "none"
    pc,       \* per device: "idle" "writing" "wrote" "done" "dead"
    writes,   \* how many times the Keychain was written for this card
    wroteOpen \* every write happened while the card was open

vars == <<card, late, claim, pc, writes, wroteOpen>>

Init ==
    /\ card = "open"
    /\ late = FALSE
    /\ claim = "none"
    /\ pc = [d \in Devices |-> "idle"]
    /\ writes = 0
    /\ wroteOpen = TRUE

Tick == /\ ~late /\ late' = TRUE /\ UNCHANGED <<card, claim, pc, writes, wroteOpen>>

\* The slot and the store's claim, together: the card is open and before its deadline.
Claim(d) ==
    /\ pc[d] = "idle"
    /\ card = "open" /\ ~late
    /\ (Exclusive => claim = "none")
    /\ claim' = d
    /\ pc' = [pc EXCEPT ![d] = "writing"]
    /\ UNCHANGED <<card, late, writes, wroteOpen>>

Write(d) ==
    /\ ~AtomicCommit
    /\ pc[d] = "writing"
    /\ writes' = writes + 1
    /\ wroteOpen' = (wroteOpen /\ card = "open")
    /\ pc' = [pc EXCEPT ![d] = "wrote"]
    /\ UNCHANGED <<card, late, claim>>

Fail(d) ==
    /\ pc[d] = "writing"
    /\ pc' = [pc EXCEPT ![d] = "idle"]
    /\ claim' = IF claim = d THEN "none" ELSE claim
    /\ UNCHANGED <<card, late, writes, wroteOpen>>

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
    /\ UNCHANGED <<card, late>>

\* card_saved only answers a card that is still open.
Finish(d) ==
    /\ ~AtomicCommit
    /\ pc[d] = "wrote"
    /\ card = "open"
    /\ card' = "saved"
    /\ pc' = [pc EXCEPT ![d] = "done"]
    /\ claim' = IF claim = d THEN "none" ELSE claim
    /\ UNCHANGED <<late, writes, wroteOpen>>

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
    /\ UNCHANGED late

\* The writer's process dies: mid-write, or (non-atomic) after the key went in and before the card was saved.
Crash(d) ==
    /\ CrashPossible
    /\ pc[d] \in {"writing", "wrote"}
    /\ pc' = [pc EXCEPT ![d] = "dead"]
    /\ UNCHANGED <<card, late, claim, writes, wroteOpen>>

\* A claim ends without its holder. The core drops a claim whose pid is dead (void_dead_claims) and, with no pid, at
\* its time; a claim whose writer lives is never lapsed (LapseNeedsDeadWriter).
Lapse ==
    /\ claim # "none"
    /\ (LapseNeedsDeadWriter => pc[claim] = "dead")
    /\ claim' = "none"
    /\ UNCHANGED <<card, late, pc, writes, wroteOpen>>

Held == claim # "none"

Sweep ==
    /\ card = "open" /\ late
    /\ (ClaimBlocksClose => ~Held)
    /\ card' = "expired"
    /\ UNCHANGED <<late, claim, pc, writes, wroteOpen>>

Withdraw ==
    /\ card = "open"
    /\ (WithdrawWaits => ~Held)
    /\ card' = "withdrawn"
    /\ UNCHANGED <<late, claim, pc, writes, wroteOpen>>

Deny ==
    /\ card = "open"
    /\ (ClaimBlocksClose => ~Held)
    /\ card' = "denied"
    /\ UNCHANGED <<late, claim, pc, writes, wroteOpen>>

Next ==
    \/ Tick \/ Sweep \/ Deny \/ Withdraw \/ Lapse
    \/ \E d \in Devices : Claim(d) \/ Write(d) \/ Fail(d) \/ FailAfterWrite(d) \/ Finish(d) \/ Commit(d) \/ Crash(d)

Spec == Init /\ [][Next]_vars

TypeOK ==
    /\ card \in {"open", "saved", "denied", "expired", "withdrawn"}
    /\ writes \in 0..4

\* Keychain written => the card was open when it was written ...
WriteWhileOpen == writes > 0 => wroteOpen

\* ... and a closed (denied, expired or withdrawn) card was never written.
ClosedNeverWritten == card \in {"denied", "expired", "withdrawn"} => writes = 0

\* ... and at most one write per card.
AtMostOneWrite == writes <= 1

\* ... and it ends saved: once nothing is in flight, a written card is saved.
WrittenEndsSaved ==
    (writes = 1 /\ (\A d \in Devices : (pc[d] # "writing" /\ pc[d] # "wrote"))) => card = "saved"

\* A failed write changes nothing the card shows: it stays open and unwritten by that step.
FailLeavesOpen == [][(\E d \in Devices : Fail(d)) => (card' = card /\ writes' = writes)]_vars

=============================================================================
