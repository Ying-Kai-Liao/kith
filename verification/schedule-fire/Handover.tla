---------------------------- MODULE Handover ----------------------------
(* Schedules handover (M4-5): kith-sched + the host's routines -> the core.   *)
(* Design: the architect's 2026-11-17-schedules-handover folder (design.md). *)
(* One atomic step = one action.                                              *)
(* J1 = kith-sched cron job, due every minute.  J2 = host routine (cron),     *)
(* due every minute.  Slots are minutes 1..MaxMin.                            *)
(* Code (re-synced after #1033, #1042, #1043, #1044):                         *)
(*   KS*        kernel/bin/kith-sched tick()/_tick/_fire_due/start under      *)
(*              tick_lock (H1). Under the lock the code's order (record the   *)
(*              run, start it, save the tick last) differs from the model's,  *)
(*              but nothing else reads state.json inside the locked section.  *)
(*              With TickLock = FALSE the KS path is the code before #1033.   *)
(*   OT*, Move  an old-code tick (that path) launchd started before the       *)
(*              update moved cli/current; launchd never starts dev.kith.sched *)
(*              again while it runs (one instance per label).                 *)
(*   Step*      cutover/modules/schedules_switch.rs (host.lock, tick.lock,    *)
(*              gate condition 6 = OldWait in schedules_gate.rs, import,      *)
(*              tombstone); the anchor: schedules/anchor.rs (H2); the live    *)
(*              kith-sched run imported into sched_runs, which the core's     *)
(*              skip rule reads (schedules/running.rs, H3).                   *)
(*   Core*      schedules/fire.rs tick / plan / fire_in.                      *)
(*   RB*        cutover/modules/schedules_back.rs cancel_waiting, next_runs,  *)
(*              write_state_back (H4, "perjobfrom").                          *)
(* Who runs a core turn (the host's runner, a cron turn's run-turn process,   *)
(* a host restart mid-run) is HostRuns.tla; Begin/End here stand for it.      *)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
  MaxMin,      \* last minute of the clock
  TickLock,    \* H1: kith-sched and the step share tick.lock
  Anchor,      \* "anchor" (H2) | "wm" (J2 counts from the watermark) | "wmback" (watermark := min(tick, next_run-1))
  LiveSkip,    \* H3: an imported live kith-sched run counts as unfinished for the core's skip rule
  Rollback,    \* "perjob" (H4) | "global" (manager's variant B) | "bare" (no rewind, no skipped:rollback row) | "off"
  CoreSpan,    \* TRUE: a core fire's run covers the span of due slots that have no row; FALSE: only the latest slot
  OldTick,     \* an old-code kith-sched tick (no tick.lock: the code before #1033) that launchd started before the update moved
               \* cli/current, still running when the step starts (FALSE: no such tick, the update happened long before)
  OldWait,     \* gate condition 6 (package F): after taking tick.lock the step waits until every tick process it listed is gone
  ShaCheck     \* before_tombstone (schedules.rs): the step refuses, tombstoning nothing, when state.json changed since its import

Jobs  == {"J1", "J2"}
Slots == 1..MaxMin
Max(S) == CHOOSE x \in S : \A y \in S : y <= x
Min(S) == CHOOSE x \in S : \A y \in S : x <= y
Mx(a, b) == IF a >= b THEN a ELSE b
Mn(a, b) == IF a <= b THEN a ELSE b
Decided == {"missed", "skipped_busy", "skipped_rollback", "ks_busy"}

VARIABLES
  now,        \* the clock
  sj,         \* state.json: [tomb, tick, lastAt, lastRun]  (lastRun = run id of last[J1].pid, 0 = none)
  ks,         \* kith-sched tick in progress: [pc, tick, lastAt, lastRun, t, newRun]
  lock,       \* tick.lock holder: "none" | "ks" | "step"
  host,       \* "own" (Gate::Own) | "stopped" | "core" (Gate::Core)
  nextRun,    \* host's next_run for J2
  step,       \* operator's step: "pre" | "imp" | "chk" | "tomb" | "switched"
  wm,         \* core watermark
  anchor,     \* H2: J2's imported next_run
  anchorUsed, \* J2's first core plan done
  liveRun,    \* run id imported from state.json last (H3 watches it)
  rb,         \* rollback: "none" | "stopped" | "cancelled" | "done"
  runs,       \* every run ever: [job, sl (slots covered), by, st: queued|running|done|cancelled]
  rows,       \* decided rows [job, slot, kind]; kind "ks_busy" = kith-sched's own 'still running' skip (not in the core's table)
  h,          \* per job: the last minute a scheduler evaluated it
  unrecorded, \* kith-sched runs started but never recorded in state.json (diagnostic)
  ot,         \* the old-code tick (OldTick): [pc, tick, lastAt, lastRun, t, newRun]; pc "idle" = not started, "gone" = ended
  moved,      \* the update has moved cli/current: launchd starts new code from here on (TRUE from the start without OldTick)
  imp         \* state.json as the step's import read it (its sha, recorded for before_tombstone)

vars == <<now, sj, ks, lock, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded, ot, moved, imp>>

RunIds == 1..Len(runs)
Unfinished(rns, i) == rns[i].st \in {"queued", "running"}
HasRow(j, s, rws) == \E r \in rws : r.job = j /\ r.slot = s /\ r.kind # "ks_busy"

Init ==
  /\ now = 0
  /\ sj = [tomb |-> FALSE, tick |-> 0, lastAt |-> 0, lastRun |-> 0]
  /\ ks = [pc |-> "idle", tick |-> 0, lastAt |-> 0, lastRun |-> 0, t |-> 0, newRun |-> 0]
  /\ lock = "none"
  /\ host = "own"
  /\ nextRun = 1
  /\ step = "pre"
  /\ wm = 0 /\ anchor = 0 /\ anchorUsed = FALSE /\ liveRun = 0
  /\ rb = "none"
  /\ runs = << >>
  /\ rows = {}
  /\ h = [j \in Jobs |-> 0]
  /\ unrecorded = {}
  /\ ot = [pc |-> "idle", tick |-> 0, lastAt |-> 0, lastRun |-> 0, t |-> 0, newRun |-> 0]
  /\ moved = ~OldTick
  /\ imp = sj

Clock == now < MaxMin /\ now' = now + 1 /\ UNCHANGED <<imp, ot, moved, sj, ks, lock, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded>>

(* ---------------- kith-sched tick ---------------- *)
KSFree == IF lock = "ks" THEN "none" ELSE lock
KSIdle == [ks EXCEPT !.pc = "idle"]

\* launchd starts dev.kith.sched again only once the previous tick has exited (one instance per label): a new-code tick
\* never overlaps the old one. OldGone: the old tick is not running (never started, or ended).
OldGone == ot.pc \in {"idle", "gone"}

KSLock ==                                   \* H1
  /\ TickLock /\ ks.pc = "idle" /\ lock = "none" /\ moved /\ OldGone
  /\ lock' = "ks" /\ ks' = [ks EXCEPT !.pc = "chk"]
  /\ UNCHANGED <<imp, ot, moved, now, sj, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded>>

KSCheck ==                                  \* H1: tombstone check under the lock
  /\ ks.pc = "chk"
  /\ IF sj.tomb THEN ks' = KSIdle /\ lock' = KSFree
                ELSE ks' = [ks EXCEPT !.pc = "read"] /\ UNCHANGED lock
  /\ UNCHANGED <<imp, ot, moved, now, sj, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded>>

KSRead ==                                   \* machine(): fails closed on a tombstone; reads tick + last; fixes its now
  /\ ks.pc = (IF TickLock THEN "read" ELSE "idle")
  /\ TickLock \/ (moved /\ OldGone)
  /\ ~sj.tomb
  /\ ks' = [pc |-> "save1", tick |-> sj.tick, lastAt |-> sj.lastAt, lastRun |-> sj.lastRun, t |-> now, newRun |-> 0]
  /\ UNCHANGED <<imp, ot, moved, now, sj, lock, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded>>

KSSave1 ==                                  \* save_machine(): tombstone check ...
  /\ ks.pc = "save1"
  /\ IF sj.tomb THEN ks' = KSIdle /\ lock' = KSFree
                ELSE ks' = [ks EXCEPT !.pc = "save2"] /\ UNCHANGED lock
  /\ UNCHANGED <<imp, ot, moved, now, sj, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded>>

KSSave2 ==                                  \* ... then rename (tick := now) over whatever is there
  /\ ks.pc = "save2"
  /\ sj' = [tomb |-> FALSE, tick |-> ks.t, lastAt |-> ks.lastAt, lastRun |-> ks.lastRun]
  /\ ks' = [ks EXCEPT !.pc = "start"]
  /\ UNCHANGED <<imp, ot, moved, now, lock, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded>>

KSStart ==
  /\ ks.pc = "start"
  /\ LET due == {s \in Slots : ks.tick < s /\ s <= ks.t} IN
     IF due = {} THEN
        /\ ks' = KSIdle /\ lock' = KSFree /\ h' = [h EXCEPT !["J1"] = Mx(@, ks.t)]
        /\ UNCHANGED <<runs, rows>>
     ELSE IF ks.lastRun # 0 /\ runs[ks.lastRun].st = "running" THEN   \* 'still running' skip (today's, documented)
        /\ rows' = rows \cup {[job |-> "J1", slot |-> s, kind |-> "ks_busy"] : s \in due}
        /\ ks' = KSIdle /\ lock' = KSFree /\ h' = [h EXCEPT !["J1"] = Mx(@, ks.t)]
        /\ UNCHANGED runs
     ELSE
        /\ runs' = Append(runs, [job |-> "J1", sl |-> due, by |-> "ks", st |-> "running"])
        /\ ks' = [ks EXCEPT !.pc = "rec1", !.newRun = Len(runs) + 1]
        /\ UNCHANGED <<lock, rows, h>>
  /\ UNCHANGED <<imp, ot, moved, now, sj, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, unrecorded>>

KSRec1 ==                                   \* record: tombstone check ...
  /\ ks.pc = "rec1"
  /\ IF sj.tomb THEN ks' = KSIdle /\ lock' = KSFree /\ unrecorded' = unrecorded \cup {ks.newRun}
                ELSE ks' = [ks EXCEPT !.pc = "rec2"] /\ UNCHANGED <<lock, unrecorded>>
  /\ UNCHANGED <<imp, ot, moved, now, sj, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h>>

KSRec2 ==                                   \* ... then rename: last = {at: now, pid}
  /\ ks.pc = "rec2"
  /\ sj' = [tomb |-> FALSE, tick |-> ks.t, lastAt |-> ks.t, lastRun |-> ks.newRun]
  /\ ks' = KSIdle /\ lock' = KSFree /\ h' = [h EXCEPT !["J1"] = Mx(@, ks.t)]
  /\ UNCHANGED <<imp, ot, moved, now, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, unrecorded>>

(* ---------------- the old-code tick (OldTick, SHOULD 7 of the F review) ---------------- *)
(* kith-sched as it was before #1033 (1cf2b5f44): no tick.lock. tick() reads state.json (a tombstone stops it), saves   *)
(* the tick (a tombstone check, then the rename over whatever is there), starts the due job, then records it (the same    *)
(* check and rename). It is the KS path with TickLock = FALSE, run by a second process that launchd started before the   *)
(* update moved cli/current: a tick of the old code that is still running when the step takes tick.lock, which it never *)
(* takes. One such tick; none starts once the update has happened.                                                      *)
OTGone == [ot EXCEPT !.pc = "gone"]

\* launchd starts the old code before the update moves cli/current. Its prune, ask sweep and wake drain come first (up to
\* about 90 s), so it may read state.json (and the clock) after the update: OTRead has no ~moved.
OTBegin ==
  /\ OldTick /\ ~moved /\ ot.pc = "idle"
  /\ ot' = [ot EXCEPT !.pc = "read"]
  /\ UNCHANGED <<imp, now, sj, ks, lock, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded, moved>>

OTRead ==                                   \* machine(): a tombstone stops it here
  /\ ot.pc = "read"
  /\ ot' = IF sj.tomb THEN OTGone
          ELSE [pc |-> "save1", tick |-> sj.tick, lastAt |-> sj.lastAt, lastRun |-> sj.lastRun, t |-> now, newRun |-> 0]
  /\ UNCHANGED <<imp, now, sj, ks, lock, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded, moved>>

OTSave1 ==
  /\ ot.pc = "save1"
  /\ ot' = IF sj.tomb THEN OTGone ELSE [ot EXCEPT !.pc = "save2"]
  /\ UNCHANGED <<imp, now, sj, ks, lock, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded, moved>>

OTSave2 ==                                  \* the rename: a tombstone written since OTSave1 is replaced
  /\ ot.pc = "save2"
  /\ sj' = [tomb |-> FALSE, tick |-> ot.t, lastAt |-> ot.lastAt, lastRun |-> ot.lastRun]
  /\ ot' = [ot EXCEPT !.pc = "start"]
  /\ UNCHANGED <<imp, now, ks, lock, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded, moved>>

OTStart ==
  /\ ot.pc = "start"
  /\ LET due == {s \in Slots : ot.tick < s /\ s <= ot.t} IN
     IF due = {} THEN
        /\ ot' = OTGone /\ h' = [h EXCEPT !["J1"] = Mx(@, ot.t)]
        /\ UNCHANGED <<runs, rows>>
     ELSE IF ot.lastRun # 0 /\ runs[ot.lastRun].st = "running" THEN
        /\ rows' = rows \cup {[job |-> "J1", slot |-> s, kind |-> "ks_busy"] : s \in due}
        /\ ot' = OTGone /\ h' = [h EXCEPT !["J1"] = Mx(@, ot.t)]
        /\ UNCHANGED runs
     ELSE
        /\ runs' = Append(runs, [job |-> "J1", sl |-> due, by |-> "ks", st |-> "running"])
        /\ ot' = [ot EXCEPT !.pc = "rec1", !.newRun = Len(runs) + 1]
        /\ UNCHANGED <<rows, h>>
  /\ UNCHANGED <<imp, now, sj, ks, lock, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, unrecorded, moved>>

OTRec1 ==
  /\ ot.pc = "rec1"
  /\ IF sj.tomb THEN ot' = OTGone /\ unrecorded' = unrecorded \cup {ot.newRun}
                ELSE ot' = [ot EXCEPT !.pc = "rec2"] /\ UNCHANGED unrecorded
  /\ UNCHANGED <<imp, now, sj, ks, lock, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, moved>>

OTRec2 ==
  /\ ot.pc = "rec2"
  /\ sj' = [tomb |-> FALSE, tick |-> ot.t, lastAt |-> ot.t, lastRun |-> ot.newRun]
  /\ ot' = OTGone /\ h' = [h EXCEPT !["J1"] = Mx(@, ot.t)]
  /\ UNCHANGED <<imp, now, ks, lock, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, unrecorded, moved>>

\* The update moves cli/current: any time, also while the old tick runs.
Move ==
  /\ ~moved /\ moved' = TRUE
  /\ UNCHANGED <<imp, now, sj, ks, lock, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded, ot>>

(* ---------------- host (Gate::Own) ---------------- *)
HostFire ==   \* next_run <= now: one run covering [next_run..now]; waits while a J2 turn is unfinished (host queue)
  /\ host = "own" /\ nextRun <= now
  /\ ~\E i \in RunIds : runs[i].job = "J2" /\ Unfinished(runs, i)
  /\ runs' = Append(runs, [job |-> "J2", sl |-> nextRun..now, by |-> "host", st |-> "running"])
  /\ nextRun' = now + 1
  /\ h' = [h EXCEPT !["J2"] = Mx(@, now)]
  /\ UNCHANGED <<imp, ot, moved, now, sj, ks, lock, host, step, wm, anchor, anchorUsed, liveRun, rb, rows, unrecorded>>

HostStop ==   \* stopped for the step, possibly long before it
  /\ host = "own" /\ step = "pre" /\ rb = "none"
  /\ host' = "stopped"
  /\ UNCHANGED <<imp, ot, moved, now, sj, ks, lock, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded>>

(* ---------------- the step ---------------- *)
StepLock ==                                  \* the operator runs the step with the updated CLI (gate condition 5)
  /\ TickLock /\ step = "pre" /\ host = "stopped" /\ lock = "none" /\ moved
  /\ lock' = "step" /\ step' = "imp"
  /\ UNCHANGED <<imp, ot, moved, now, sj, ks, host, nextRun, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded>>

\* Gate condition 6 (OldWait; schedules_gate.rs, package F): right after taking tick.lock the step lists this home's tick
\* processes and waits until each is gone. At TICK_WAIT it refuses and changes nothing, so the refusal is no action here.
StepImport ==
  /\ step = (IF TickLock THEN "imp" ELSE "pre") /\ host = "stopped" /\ ~sj.tomb /\ moved
  /\ OldWait => OldGone
  /\ wm' = IF Anchor = "wmback" THEN Mn(sj.tick, nextRun - 1) ELSE sj.tick
  /\ rows' = rows \cup (IF sj.lastRun # 0 THEN {[job |-> "J1", slot |-> sj.lastAt, kind |-> "fired"]} ELSE {})
  /\ liveRun' = sj.lastRun
  /\ anchor' = nextRun
  /\ step' = "chk" /\ imp' = sj
  /\ UNCHANGED <<ot, moved, now, sj, ks, lock, host, nextRun, anchorUsed, rb, runs, h, unrecorded>>

\* before_tombstone: state.json's sha against the import's. A refusal leaves the module stepping and frees the locks; the
\* operator runs the step again, which imports again.
StepCheck ==
  /\ step = "chk"
  /\ IF ~ShaCheck \/ sj = imp
       THEN step' = "tomb" /\ UNCHANGED lock
       ELSE step' = "pre" /\ lock' = (IF lock = "step" THEN "none" ELSE lock)
  /\ UNCHANGED <<imp, ot, moved, now, sj, ks, host, nextRun, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded>>

StepTomb ==
  /\ step = "tomb"
  /\ sj' = [sj EXCEPT !.tomb = TRUE]
  /\ lock' = IF lock = "step" THEN "none" ELSE lock
  /\ step' = "switched"
  /\ UNCHANGED <<imp, ot, moved, now, ks, host, nextRun, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded>>

HostCore ==   \* host restarts with Gate::Core
  /\ step = "switched" /\ host = "stopped" /\ rb = "none"
  /\ host' = "core"
  /\ UNCHANGED <<imp, ot, moved, now, sj, ks, lock, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, runs, rows, h, unrecorded>>

(* ---------------- the core tick ---------------- *)
From(j) == IF j = "J2" /\ Anchor = "anchor" /\ ~anchorUsed THEN anchor - 1 ELSE wm

CoreJob(j, rws, rns) ==
  LET f     == From(j)
      due   == {s \in Slots : f < s /\ s <= now}
      fresh == {s \in due : ~HasRow(j, s, rws)}
      busy  == \E i \in 1..Len(rns) : rns[i].job = j /\ Unfinished(rns, i)
                                     /\ (rns[i].by # "ks" \/ (LiveSkip /\ i = liveRun))
  IN IF due = {} THEN [rows |-> rws, runs |-> rns]
     ELSE LET L      == Max(due)
              missed == {[job |-> j, slot |-> s, kind |-> "missed"] : s \in fresh \ {L}}
          IN IF HasRow(j, L, rws) THEN [rows |-> rws \cup missed, runs |-> rns]
             ELSE IF busy THEN [rows |-> rws \cup missed \cup {[job |-> j, slot |-> L, kind |-> "skipped_busy"]}, runs |-> rns]
             ELSE [rows |-> rws \cup missed \cup {[job |-> j, slot |-> L, kind |-> "fired"]},
                   runs |-> Append(rns, [job |-> j, sl |-> IF CoreSpan THEN fresh ELSE {L}, by |-> "core", st |-> "queued"])]

CoreTick ==
  /\ host = "core"
  /\ now > wm \/ (Anchor = "anchor" /\ ~anchorUsed)
  /\ LET r1 == CoreJob("J1", rows, runs)
         r2 == CoreJob("J2", r1.rows, r1.runs)
     IN rows' = r2.rows /\ runs' = r2.runs
  /\ wm' = Mx(wm, now)
  /\ anchorUsed' = TRUE
  /\ h' = [j \in Jobs |-> Mx(h[j], now)]
  /\ UNCHANGED <<imp, ot, moved, now, sj, ks, lock, host, nextRun, step, anchor, liveRun, rb, unrecorded>>

Begin ==      \* a queued core turn begins (the host runs turns)
  /\ host = "core"
  /\ \E i \in RunIds : runs[i].st = "queued" /\ runs' = [runs EXCEPT ![i].st = "running"]
  /\ UNCHANGED <<imp, ot, moved, now, sj, ks, lock, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, rows, h, unrecorded>>

End ==
  /\ \E i \in RunIds : runs[i].st = "running" /\ runs' = [runs EXCEPT ![i].st = "done"]
  /\ UNCHANGED <<imp, ot, moved, now, sj, ks, lock, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rb, rows, h, unrecorded>>

(* ---------------- rollback ---------------- *)
RBStop ==
  /\ Rollback # "off" /\ rb = "none" /\ host = "core"
  /\ host' = "stopped" /\ rb' = "stopped"
  /\ UNCHANGED <<imp, ot, moved, now, sj, ks, lock, nextRun, step, wm, anchor, anchorUsed, liveRun, runs, rows, h, unrecorded>>

RBCancel ==   \* cancel every queued, not yet begun core turn
  /\ rb = "stopped"
  /\ runs' = [i \in RunIds |-> IF runs[i].st = "queued" THEN [runs[i] EXCEPT !.st = "cancelled"] ELSE runs[i]]
  /\ rb' = "cancelled"
  /\ UNCHANGED <<imp, ot, moved, now, sj, ks, lock, host, nextRun, step, wm, anchor, anchorUsed, liveRun, rows, h, unrecorded>>

RBWrite ==
  /\ rb = "cancelled"
  /\ LET C   == {i \in RunIds : runs[i].st = "cancelled"}
         CS  == {Max(runs[i].sl) : i \in C}
         C1  == {Max(runs[i].sl) : i \in {k \in C : runs[k].job = "J1"}}
         C2  == {Max(runs[i].sl) : i \in {k \in C : runs[k].job = "J2"}}
         tk  == IF Rollback = "global" /\ CS # {} THEN Min(CS) - 1 ELSE wm
         PJ  == Rollback \in {"perjob", "perjobfrom", "perjobnolast"}
         \* FixN: next_run from the unused anchor (global gets it too, so its row isolates the global rewind);
         \* FixL: a running core J1 run goes into state.json's last ("perjobnolast" drops only this).
         FixN == Rollback \in {"perjobfrom", "perjobnolast", "global"}
         FixL == Rollback \in {"perjobfrom", "global"}
         \* perjobfrom (H4 fixed): J2 counts from where it would count next (anchor until used, never below anchor-1),
         \* and a core J1 run still running goes into state.json's last so kith-sched's 'still running' rule sees it.
         \* (first try Mx(wm, anchor-1)+1 lost J2's slots when the host had been down: the unused anchor is the slot itself)
         j2n == IF Anchor = "anchor" /\ ~anchorUsed THEN anchor ELSE wm + 1
         nr  == CASE Rollback = "global" /\ CS # {} -> Min(CS)
                  [] PJ /\ C2 # {} -> Min(C2)
                  [] FixN -> j2n
                  [] OTHER -> wm + 1
         RJ1 == {i \in RunIds : runs[i].job = "J1" /\ runs[i].by = "core" /\ runs[i].st = "running"}
         lr  == IF FixL /\ RJ1 # {} THEN Max(RJ1) ELSE sj.lastRun
         cj  == {<<"J1", c>> : c \in C1} \cup {<<"J2", c>> : c \in C2}
     IN /\ sj' = [tomb |-> FALSE, tick |-> tk, lastAt |-> sj.lastAt, lastRun |-> lr]
        /\ nextRun' = nr
        /\ rows' = IF PJ
                   THEN {r \in rows : ~(<<r.job, r.slot>> \in cj /\ r.kind = "fired")}
                        \cup {[job |-> p[1], slot |-> p[2], kind |-> "skipped_rollback"] : p \in cj}
                   ELSE rows
  /\ host' = "own" /\ rb' = "done"
  /\ UNCHANGED <<imp, ot, moved, now, ks, lock, step, wm, anchor, anchorUsed, liveRun, runs, h, unrecorded>>

Next ==
  \/ Clock
  \/ KSLock \/ KSCheck \/ KSRead \/ KSSave1 \/ KSSave2 \/ KSStart \/ KSRec1 \/ KSRec2
  \/ HostFire \/ HostStop
  \/ StepLock \/ StepImport \/ StepCheck \/ StepTomb \/ HostCore
  \/ CoreTick \/ Begin \/ End
  \/ RBStop \/ RBCancel \/ RBWrite
  \/ OTBegin \/ OTRead \/ OTSave1 \/ OTSave2 \/ OTStart \/ OTRec1 \/ OTRec2 \/ Move

Spec == Init /\ [][Next]_vars

(* ---------------- properties ---------------- *)
Live(i) == runs[i].st # "cancelled"
Covers(j, s) == {i \in RunIds : runs[i].job = j /\ Live(i) /\ s \in runs[i].sl}

AtMostOnce == \A j \in Jobs, s \in Slots : Cardinality(Covers(j, s)) <= 1

NoStack == \A j \in Jobs : Cardinality({i \in RunIds : runs[i].job = j /\ runs[i].st = "running"}) <= 1

Covered(j, s) == Covers(j, s) # {} \/ \E r \in rows : r.job = j /\ r.slot = s /\ r.kind \in Decided

(* Quiet: the clock is at its end, no tick/step/rollback is half done, every run has ended,  *)
(* and every scheduler that is still active has caught up to now (nothing left it would do). *)
Quiet ==
  /\ now = MaxMin
  /\ ks.pc = "idle" /\ OldGone
  /\ step \in {"pre", "switched"}
  /\ rb \in {"none", "done"}
  /\ (step = "switched" /\ rb = "none") => host = "core"
  /\ \A i \in RunIds : runs[i].st \in {"done", "cancelled"}
  /\ ~sj.tomb => sj.tick >= MaxMin
  /\ host = "own" => nextRun > MaxMin
  /\ host = "core" => (wm >= MaxMin /\ (Anchor = "anchor" => anchorUsed))

NoSilentLoss == Quiet => \A j \in Jobs : \A s \in Slots : s <= h[j] => Covered(j, s)

(* diagnostics *)
AllRecorded == unrecorded = {}
TombHolds == (step = "switched" /\ rb = "none") => sj.tomb
\* reachability sanity (expected to FAIL = the state is reachable)
NotQuietAfterRollback == ~(Quiet /\ rb = "done")
NotQuietSwitched == ~(Quiet /\ step = "switched" /\ rb = "none")
=============================================================================
