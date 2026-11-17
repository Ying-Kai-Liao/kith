---- MODULE HostRuns ----
\* Who runs a turn the core queued. The architect's model of design 2026-11-16-core-queued-runner (his folder holds the
\* original and its results), copied here for the M4-5 re-sync, with the cron turn of package C (#1044) added.
\*
\* Three turns: "f" is fired by the core's schedule tick (no enq_key, conversation schedule:<agent>:<job>:<engine>) for a
\* routine, and runs on the host's bot actor (core_runs.rs); "h" is begun by a host actor (hostclaim.enqueue, enq_key
\* bound to that host run); "c" (Cron) is fired for a job of kith-sched and runs as its own process, `kith-sched
\* run-turn` (core_cron.rs, conversations/sched_run.rs). One host per Mac; it can go down and come back (memory lost,
\* adopt at the first Writer begin). The core can restart (generation bump, holder hello, step 5 reconcile). A claim is
\* one row per attempt; the core's claim_next is a CAS queued -> claimed, the redeem a CAS claimed -> running. Bounded:
\* one core restart, one host restart. The core cannot see whether the host is up, only whether the claim's recorded
\* process is alive: an engine outlives a host that died (its stub keeps it; a cron run is its own process group).
\*
\* Model action -> code:
\*   Fire, FireC            schedules/fire.rs fire_in: the turn, queued with no enq_key
\*   HostEnqueue            hostclaim.enqueue from a host actor (keyed)
\*   Claim, SkipIt          serve/host/src/routines/core_runs.rs take/pull (hostclaim.queued, redeemedBefore -> cancel
\*                          "interrupted"), the actor's begin by id (claim_next CAS)
\*   Redeem, Prompt, Finish the bot run's redeem, its prompt, the host's `done` report
\*   SeeDied                writer_turn.rs cancel_reason: End::Died cancels the turn
\*   CoreRestart            conversations/reconcile.rs release_unlisted, step 5; rule K = KeepLiveHost (the host's claimed
\*                          claims and its running ones on a live process are kept; `external` claims are never touched
\*                          unless `ending`)
\*   Kill, EndGone          claims.rs kill_resend, end_gone
\*   HostDown, HostUp       the host process ending and starting (a new host instance)
\*   Adopt, Strays          conversations/warm.rs adopt (B5: a claimed claim with no pid ends and its turn is requeued; one
\*                          with a pid goes to ending, requeued, killed; `external` claims are left alone); hostclaim.strays
\*   Expire                 the deadline (q218)
\*   ClaimC                 core_cron.rs Live::cron -> CoreWriter::claim_run -> sched_run::claim_run: kind `turn`, no process
\*   SpawnC                 core_cron.rs spawn: `kith-sched run-turn AGENT JOB --turn N --claim C`, own process group
\*   AttachC, LoseC         kith-sched run_turn -> claim.attach (sched_run::attach): the check-and-set kind `turn`,
\*                          `claimed`, no pid -> `external`, `running`, pid + start, BEFORE the exec of the engine; refused
\*                          (exit 4: nothing ran) when the claim is not that one any more
\*   DieC                   run-turn exiting before it attaches (RunDies)
\*   EndC                   the engine (`kith AGENT task`, the same pid) exits
\*   ReportC                core_cron.rs `over`: the host thread that waits for the child cancels a turn that never attached
\*                          (cancel_waiting: a queued turn or a claimed one with no pid) and reports done/exit (claims.rs
\*                          apply: `done` -> turn done; an external claim's bad exit -> turn failed, never requeued)
\*   EndGoneC               claims.rs end_gone: an external claim whose process is gone ends, its turn failed
EXTENDS Naturals, FiniteSets

CONSTANTS Runner,          \* the host pulls core-queued turns it did not begin
          PullOnlyUnkeyed, \* ... only turns with no enq_key (never a turn a host actor began)
          SkipRedeemed,    \* ... a turn whose earlier claim was redeemed (its prompt may have gone) is cancelled, not run
          KeepLiveHost,    \* step 5 leaves host-instance claims alone (their process is alive); FALSE = the code before K
          Cron,            \* the turn "c" exists: a kith-sched job's fired turn, run by `kith-sched run-turn` (package C)
          AttachCAS,       \* run-turn attaches with the check-and-set on its own claim (claim id, `claimed`, no pid);
                           \* FALSE: it attaches to the turn's current claim whatever it is (the break the CAS prevents)
          MaxC,            \* claims of "c" at most (bounds the model; one per host start and core restart is enough)
          RunDies          \* a run-turn process may exit before it attaches while its claim is still its own (an error, a
                           \* kill, a failed exec): DieC

HTurns == {"f", "h"}
Turns == IF Cron THEN HTurns \cup {"c"} ELSE HTurns
Final == {"done", "cancelled", "expired", "failed"}

VARIABLES ts,       \* turn state: none, queued, claimed, running, done, cancelled, expired, failed
          cl,       \* the turn's current claim: none, claimed, running, ending, ended
          red,      \* some claim of this turn was redeemed (an engine was handed it; for "c": a run attached)
          sent,     \* prompts sent for the turn, over all its claims
          psent,    \* the current claim's prompt went
          eng,      \* an engine is alive working the turn
          owner,    \* the host run that holds the turn's item: "host" (alive, knows it) or "none"
          up,       \* the host is up
          crs, hrs, \* restarts left
          adopted,  \* the host since its start ran adopt
          ev,
          cid,      \* "c": the number of its current claim (0: never claimed)
          sp,       \* "c": the claims whose run-turn process was spawned and has not attached or given up yet
          thr,      \* "c": the claim the live host's `over` thread waits on (0: none; a host restart loses it). One only:
                    \* where a newer claim spawns while an older `over` still waits (hr-no-keep), the older one drops
                    \* out of the model
          pa        \* "c": the claim a run attached to (0: none)

vars == <<ts, cl, red, sent, psent, eng, owner, up, crs, hrs, adopted, ev, cid, sp, thr, pa>>
cvars == <<cid, sp, thr, pa>>

Init == /\ ts = [t \in Turns |-> "none"] /\ cl = [t \in Turns |-> "none"] /\ red = [t \in Turns |-> FALSE]
        /\ sent = [t \in Turns |-> 0] /\ psent = [t \in Turns |-> FALSE] /\ eng = [t \in Turns |-> FALSE]
        /\ owner = [t \in Turns |-> "none"] /\ up = TRUE /\ crs = 1 /\ hrs = 1 /\ adopted = TRUE /\ ev = {}
        /\ cid = 0 /\ sp = {} /\ thr = 0 /\ pa = 0

U(x) == UNCHANGED x

\* The core's tick fires the job: a queued turn with no owner.
Fire == ts["f"] = "none" /\ ts' = [ts EXCEPT !["f"] = "queued"]
        /\ U(<<cl, red, sent, psent, eng, owner, up, crs, hrs, adopted, ev, cvars>>)

\* A host actor accepts a message and enqueues its turn (it owns the item).
HostEnqueue == up /\ ts["h"] = "none" /\ ts' = [ts EXCEPT !["h"] = "queued"] /\ owner' = [owner EXCEPT !["h"] = "host"]
               /\ U(<<cl, red, sent, psent, eng, up, crs, hrs, adopted, ev, cvars>>)

\* A turn the host may start: its own (the owner begins it again by id), or, with the runner, a core-queued one.
MayPull(t) == Runner /\ owner[t] = "none" /\ (PullOnlyUnkeyed => t \in {"f", "c"})
Starts(t) == up /\ adopted /\ ts[t] = "queued" /\ (owner[t] = "host" \/ MayPull(t)) /\ (SkipRedeemed => ~red[t])

\* claim_next by turn id: CAS queued -> claimed. The runner's pull makes the host the item's owner.
Claim(t) == Starts(t) /\ cl[t] \in {"none", "ended"}
            /\ ts' = [ts EXCEPT ![t] = "claimed"] /\ cl' = [cl EXCEPT ![t] = "claimed"]
            /\ owner' = [owner EXCEPT ![t] = "host"] /\ psent' = [psent EXCEPT ![t] = FALSE]
            /\ U(<<red, sent, eng, up, crs, hrs, adopted, ev, cvars>>)

\* The runner's rule for a turn whose earlier claim was redeemed: cancelled ("interrupted"), never run again.
SkipIt(t) == up /\ adopted /\ SkipRedeemed /\ ts[t] = "queued" /\ red[t] /\ MayPull(t) /\ cl[t] \in {"none", "ended"}
             /\ ts' = [ts EXCEPT ![t] = "cancelled"] /\ U(<<cl, red, sent, psent, eng, owner, up, crs, hrs, adopted, ev, cvars>>)

Redeem(t) == up /\ owner[t] = "host" /\ cl[t] = "claimed"
             /\ cl' = [cl EXCEPT ![t] = "running"] /\ ts' = [ts EXCEPT ![t] = "running"]
             /\ red' = [red EXCEPT ![t] = TRUE] /\ eng' = [eng EXCEPT ![t] = TRUE]
             /\ U(<<sent, psent, owner, up, crs, hrs, adopted, ev, cvars>>)

Prompt(t) == up /\ owner[t] = "host" /\ cl[t] = "running" /\ eng[t] /\ ~psent[t]
             /\ psent' = [psent EXCEPT ![t] = TRUE] /\ sent' = [sent EXCEPT ![t] = sent[t] + 1]
             /\ U(<<ts, cl, red, eng, owner, up, crs, hrs, adopted, ev, cvars>>)

\* The engine finishes; the host reports done: the claim ends, the turn is done.
Finish(t) == up /\ owner[t] = "host" /\ cl[t] = "running" /\ eng[t] /\ psent[t]
             /\ cl' = [cl EXCEPT ![t] = "ended"] /\ ts' = [ts EXCEPT ![t] = "done"] /\ eng' = [eng EXCEPT ![t] = FALSE]
             /\ owner' = [owner EXCEPT ![t] = "none"]
             /\ U(<<red, sent, psent, up, crs, hrs, adopted, ev, cvars>>)

\* The host's engine died under a turn it holds (killed): End::Died, the turn is cancelled (writer_turn.rs cancel_reason).
SeeDied(t) == up /\ owner[t] = "host" /\ ~eng[t] /\ red[t] /\ ts[t] \in {"queued", "running"} /\ cl[t] \in {"ending", "ended"}
              /\ ts' = [ts EXCEPT ![t] = "cancelled"] /\ owner' = [owner EXCEPT ![t] = "none"]
              /\ U(<<cl, red, sent, psent, eng, up, crs, hrs, adopted, ev, cvars>>)

\* An attached cron run's claim is `external`: step 5 and adopt leave it alone (end_gone ends it once its process is gone).
Attached(t) == t = "c" /\ cl[t] = "running"

\* The core restarts: step 5. Unredeemed claims end and their turn is put back (the CAS makes a late redeem fail);
\* redeemed ones go to ending, the turn is put back, the process is marked for a kill. KeepLiveHost: a host claim that is
\* `claimed` (no process yet: the host's redeem or stop decides, a dead host's goes at the next host's adopt) or `running`
\* on a process that is alive is left as it is; a running one whose process is gone is reconciled as before.
Live(t) == Attached(t) \/ (KeepLiveHost /\ (cl[t] = "claimed" \/ (cl[t] = "running" /\ eng[t])))
CoreRestart == crs > 0 /\ crs' = crs - 1
    /\ cl' = [t \in Turns |-> IF Live(t) THEN cl[t]
                              ELSE CASE cl[t] = "claimed" -> "ended" [] cl[t] = "running" -> "ending" [] OTHER -> cl[t]]
    /\ ts' = [t \in Turns |-> IF ~Live(t) /\ cl[t] \in {"claimed", "running"} THEN "queued" ELSE ts[t]]
    /\ ev' = ev \cup (IF \E t \in Turns : cl[t] = "running" /\ eng[t] /\ up /\ owner[t] = "host" /\ ~Live(t)
                      THEN {"killedlive"} ELSE {})
    /\ U(<<red, sent, psent, eng, owner, up, hrs, adopted, cvars>>)

\* kill_resend: an ending claim's process is killed; end_gone then ends the claim.
Kill(t) == cl[t] = "ending" /\ eng[t] /\ eng' = [eng EXCEPT ![t] = FALSE] /\ U(<<ts, cl, red, sent, psent, owner, up, crs, hrs, adopted, ev, cvars>>)
EndGone(t) == cl[t] = "ending" /\ ~eng[t] /\ cl' = [cl EXCEPT ![t] = "ended"] /\ U(<<ts, red, sent, psent, eng, owner, up, crs, hrs, adopted, ev, cvars>>)

\* The host goes down (its memory, the items it owned and its `over` threads, is lost; stubs, engines and run-turn
\* processes live on) and comes back.
HostDown == up /\ hrs > 0 /\ hrs' = hrs - 1 /\ up' = FALSE /\ owner' = [t \in Turns |-> "none"] /\ thr' = 0
            /\ U(<<ts, cl, red, sent, psent, eng, crs, adopted, ev, cid, sp, pa>>)
HostUp == ~up /\ up' = TRUE /\ adopted' = FALSE /\ U(<<ts, cl, red, sent, psent, eng, owner, crs, hrs, ev, cvars>>)

\* adopt (warm.rs, the first Writer begin): every host claim of an older instance: claimed -> ended + requeued,
\* running -> ending + requeued (killed by kill_resend); an `external` one (an attached cron run) is left alone. Then the
\* strays sweep: a keyed turn whose key no host run holds is cancelled (P9 2f), never run.
Adopt == up /\ ~adopted /\ adopted' = TRUE
    /\ cl' = [t \in Turns |-> IF Attached(t) THEN cl[t]
                              ELSE CASE cl[t] = "claimed" -> "ended" [] cl[t] = "running" -> "ending" [] OTHER -> cl[t]]
    /\ ts' = [t \in Turns |-> IF ~Attached(t) /\ cl[t] \in {"claimed", "running"} THEN "queued" ELSE ts[t]]
    /\ U(<<red, sent, psent, eng, owner, up, crs, hrs, ev, cvars>>)
Strays == up /\ adopted /\ ts["h"] = "queued" /\ owner["h"] = "none" /\ cl["h"] \in {"none", "ended"}
          /\ ts' = [ts EXCEPT !["h"] = "cancelled"] /\ U(<<cl, red, sent, psent, eng, owner, up, crs, hrs, adopted, ev, cvars>>)

\* A fired turn past its deadline (q218) expires; only while nobody can run it (the host is down).
Expire(t) == t \in {"f", "c"} /\ ~up /\ ts[t] = "queued" /\ ts' = [ts EXCEPT ![t] = "expired"]
             /\ U(<<cl, red, sent, psent, eng, owner, up, crs, hrs, adopted, ev, cvars>>)

\* ---- the cron turn "c" (package C) ------------------------------------------------------------------------------------

FireC == Cron /\ ts["c"] = "none" /\ ts' = [ts EXCEPT !["c"] = "queued"]
         /\ U(<<cl, red, sent, psent, eng, owner, up, crs, hrs, adopted, ev, cvars>>)

\* claim_run: the pulled turn is claimed with no process (kind `turn`); `owner` is the runner's call between the claim and
\* the spawn (a host that dies in between leaves the claim to the next host's adopt).
ClaimC == Cron /\ Starts("c") /\ owner["c"] = "none" /\ cl["c"] \in {"none", "ended"} /\ cid < MaxC
          /\ ts' = [ts EXCEPT !["c"] = "claimed"] /\ cl' = [cl EXCEPT !["c"] = "claimed"]
          /\ owner' = [owner EXCEPT !["c"] = "host"] /\ cid' = cid + 1
          /\ U(<<red, sent, psent, eng, up, crs, hrs, adopted, ev, sp, thr, pa>>)

\* The spawn (no check of the claim: it follows the claim at once); the host's `over` thread waits for the child.
SpawnC == up /\ owner["c"] = "host"
          /\ sp' = sp \cup {cid} /\ thr' = cid /\ owner' = [owner EXCEPT !["c"] = "none"]
          /\ U(<<ts, cl, red, sent, psent, eng, up, crs, hrs, adopted, ev, cid, pa>>)

\* What claim.attach accepts from the process spawned for claim k.
CASOk(k) == IF AttachCAS THEN k = cid /\ cl["c"] = "claimed" ELSE ts["c"] \in {"claimed", "running"}

\* The run attaches, then execs the engine, which sends the prompt (counted here: from the attach on it may have gone).
\* Runs whether the host is up or not.
AttachC == \E k \in sp : CASOk(k)
          /\ sp' = sp \ {k} /\ pa' = k
          /\ cl' = [cl EXCEPT !["c"] = "running"] /\ ts' = [ts EXCEPT !["c"] = "running"]
          /\ red' = [red EXCEPT !["c"] = TRUE] /\ eng' = [eng EXCEPT !["c"] = TRUE] /\ sent' = [sent EXCEPT !["c"] = @ + 1]
          /\ U(<<psent, owner, up, crs, hrs, adopted, ev, cid, thr>>)

\* Refused (exit 4, nothing ran). The live host's thread for that claim cancels the turn if it is still waiting (cancel by
\* turn: a queued turn, or a claimed one with no pid), then its report finds the claim gone.
LoseC == \E k \in sp : ~CASOk(k)
          /\ sp' = sp \ {k}
          /\ (IF up /\ thr = k /\ (ts["c"] = "queued" \/ (ts["c"] = "claimed" /\ cl["c"] = "claimed"))
               THEN /\ ts' = [ts EXCEPT !["c"] = "cancelled"]
                    /\ cl' = [cl EXCEPT !["c"] = IF @ = "claimed" THEN "ended" ELSE @]
               ELSE U(<<ts, cl>>))
          /\ thr' = (IF thr = k THEN 0 ELSE thr)
          /\ U(<<red, sent, psent, eng, owner, up, crs, hrs, adopted, ev, cid, pa>>)

\* The engine (the run-turn process itself, after its exec) exits.
EndC == eng["c"] /\ eng' = [eng EXCEPT !["c"] = FALSE] /\ U(<<ts, cl, red, sent, psent, owner, up, crs, hrs, adopted, ev, cvars>>)

\* The host's thread sees its child exit and reports done (a bad exit is `failed`; both final, only `done` is modelled).
ReportC == up /\ thr # 0 /\ thr = pa /\ ~eng["c"] /\ thr' = 0
          /\ (IF cl["c"] = "running" /\ pa = cid
                THEN cl' = [cl EXCEPT !["c"] = "ended"] /\ ts' = [ts EXCEPT !["c"] = "done"]
                ELSE U(<<ts, cl>>))
          /\ U(<<red, sent, psent, eng, owner, up, crs, hrs, adopted, ev, cid, sp, pa>>)

\* end_gone: the attached run's process is gone and no report came (the host was restarted): the turn fails.
EndGoneC == cl["c"] = "running" /\ pa = cid /\ ~eng["c"]
          /\ cl' = [cl EXCEPT !["c"] = "ended"] /\ ts' = [ts EXCEPT !["c"] = "failed"]
          /\ U(<<red, sent, psent, eng, owner, up, crs, hrs, adopted, ev, cvars>>)

\* run-turn exits before it attaches while its claim is still its own (exit 2/3/5, a kill, a failed exec): the live
\* host's thread cancels the turn as it does for LoseC; a dead host's claim goes at the next adopt. Architect's action
\* (review of #1047). "died" marks the behaviour so FiredRuns does not count such a turn as one that should have run.
DieC == RunDies /\ \E k \in sp : CASOk(k)
          /\ sp' = sp \ {k}
          /\ (IF up /\ thr = k /\ (ts["c"] = "queued" \/ (ts["c"] = "claimed" /\ cl["c"] = "claimed"))
               THEN /\ ts' = [ts EXCEPT !["c"] = "cancelled"]
                    /\ cl' = [cl EXCEPT !["c"] = IF @ = "claimed" THEN "ended" ELSE @]
               ELSE U(<<ts, cl>>))
          /\ thr' = (IF thr = k THEN 0 ELSE thr)
          /\ ev' = ev \cup {"died"}
          /\ U(<<red, sent, psent, eng, owner, up, crs, hrs, adopted, cid, pa>>)

CronNext == Cron /\ (FireC \/ ClaimC \/ SpawnC \/ AttachC \/ LoseC \/ EndC \/ ReportC \/ EndGoneC \/ DieC)

Next == Fire \/ HostEnqueue \/ CoreRestart \/ HostDown \/ HostUp \/ Adopt \/ Strays \/ CronNext
        \/ (\E t \in Turns : SkipIt(t) \/ Kill(t) \/ EndGone(t) \/ Expire(t))
        \/ (\E t \in HTurns : Claim(t) \/ Redeem(t) \/ Prompt(t) \/ Finish(t) \/ SeeDied(t))

Spec == Init /\ [][Next]_vars

\* A turn's prompt goes to an engine at most once (no job runs twice for one slot, no message is answered twice).
PromptOnce == \A t \in Turns : sent[t] <= 1

\* A core restart never kills a live host turn (the person's in-flight message survives a daemon restart).
NoLiveKill == "killedlive" \notin ev

\* Nothing left to happen: every turn that exists is final and no claim is open (the Quiet form: nothing enabled).
Quiet == ~ENABLED Next
NoStuck == Quiet => \A t \in Turns : (ts[t] \in Final \cup {"none"}) /\ cl[t] \in {"none", "ended"}

\* A fired turn the host was up to run, and that was never handed to an engine, is not expired or cancelled.
FiredRuns == ("died" \notin ev /\ Quiet) => \A t \in {"f", "c"} \cap Turns :
                        (ts[t] # "none" => ts[t] \in {"done", "expired", "failed"} \/ red[t])
====
