---------------------------- MODULE Threads ----------------------------
(***************************************************************************)
(* One thread = one Claude session. Who may write to it, and when.         *)
(*                                                                         *)
(* Writers: a terminal `kith <agent>` or a one-shot (its claude process    *)
(* holds the session while it lives), the host's turn actor (an app send), *)
(* and a bot guest's `kith ask --thread` (behind BotEnabled). Each takes   *)
(* the session's writer claim (C = terminal/claim.rs, kith-sessions        *)
(* take_claim): one file per session id, created exclusively.              *)
(*                                                                         *)
(* CoreWriter = TRUE is the M3 cutover (design 2026-11-14-m3-cutover): no  *)
(* claim file. The claim is a core row (`cl` stands for the claims table:  *)
(* hostclaim.* for the host, claim.external / terminal for kernel takers), *)
(* and the writer lock is a second, independent layer: the stub's flock    *)
(* (`lk`), taken AFTER the claim by every taker. CoreWriter = FALSE is the *)
(* claim-file code, and every row written for it is unchanged.             *)
(*                                                                         *)
(* Every action names the code it models. One atomic step in the code is   *)
(* one action here; a check followed later by the act it guards is TWO     *)
(* actions, so another writer can act in between.                          *)
(*                                                                         *)
(* Code (sync refs in .claude/skills/thread-writers/SKILL.md):             *)
(*   K = kernel/bin/kith-sessions, kernel/bin/kith                         *)
(*   R = serve/host/src/terminal/registry.rs                               *)
(*   S = serve/host/src/terminal/sync.rs                                   *)
(*   T = serve/host/src/api/threads.rs, api/bots.rs (send)                 *)
(*   Q = serve/host/src/agent/bot/{queue,turn,session}.rs                  *)
(*   A = serve/host/src/hub/actors.rs (working_lanes)                      *)
(*   C = serve/host/src/terminal/claim.rs, kith-sessions take_claim        *)
(*   W = serve/host/src/terminal/core_writer.rs, agent/bot/writer*.rs      *)
(*   X = core/runtime/src/conversations/{claims,external,warm}.rs          *)
(*   U = core/stub/src/resident.rs (the stub's lock)                       *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    Terminals,        \* terminal runs that may write the thread, each run once: {"t1","t2"}
    MaxSends,         \* how many app/bot sends one run may make
    StartTerminalBorn,\* TRUE: also start from a thread born in a live terminal (t1)
    \* --- what the code does today (TRUE). FALSE = a broken variant, to show a property can fail.
    SendChecksLive,        \* S before_send_blocking: refuse a send while writer() names someone
    TakingBlocksSend,      \* S writer(): a take-over under way (`taking`) is a writer
    ResumeChecksHostBusy,  \* K resolve: refuse while resolveThread says hostBusy
    HostBusyCountsQueued,  \* S resolve: hostBusy also counts a send still queued (send_queued) and `taking`
    ResumeChecksOpen,      \* K resolve: refuse while the registry's pid is live (live_pid)
    ResumeClaims,          \* K resolve: claim() the session before the host is asked
    FreshClaims,           \* K bin/kith: a run resolve did not claim (fresh, KITH_RESUME, a one-shot) claims its
                           \* session id before claude starts (`kith-sessions terminal` / `claim`), PR #324
    StartGateChecks,       \* S start_gate (from Q turn.rs start_turn_inner): refuse a turn while a terminal holds
    HostStartCAS,          \* Q turn.rs claim_for_turn: refuse the turn when another holds the session's claim
    OffTurnWakes,          \* Q offturn.rs adopt_off_turn: an agent that woke itself (a background task) gets a host turn
                           \* in its lane with no queue entry: the same start_gate and claim_for_turn as any turn
    TakeoverRefusesBusy,   \* S take_over_inner: Live::Busy -> TerminalBusy
    TakeoverRechecks,      \* S take_over_inner: liveness again just before the signal
    ProcStartCheck,        \* R liveness/started_as: a reused pid is not the session
    ForgetsCache,          \* S forget_loaded -> Q session.rs forget_loaded: the actor drops its cached session
    ForgetOnImport,        \* S import_locked: an import that brings new lines forgets the cache, even if the
                           \* terminal was never seen live
    \* --- the core writer (CoreWriter = TRUE); each is TRUE in the code, FALSE in a broken variant
    CoreWriter,            \* TRUE: the claim is a core row and the stub's lock is the second layer
    HostTakesLock,         \* W attempt step 5: the host takes the stub's lock (`take`) before the redeem
    TermTakesLock,         \* kith-stub (bin/kith exec kith-stub) takes the lock when claude starts
    TermRegistersClaim,    \* K claim / terminal: the claim row (claim.external) is taken before claude starts
    ClaimNextChecksHeld,   \* X claims.rs claim_next: a live claim of anyone else answers `held`
    GateRead,              \* W begin: the gate is read first; not ok:true is Held (the core is asked nothing)
    SettlesOwnEnding,      \* W begin: an `ending` turn claim whose stub is gone is settled, then the begin claims
    DiedReports,           \* W finish End::Died: the end is reported (`exit`), so the claim ends
    FailoverReclaims,      \* writer_turn: a move to the next account begins a NEW core turn (a new claim and lock)
    AbandonKeepsLock,      \* TRUE in the code: an abandon keeps the stub's lock until the stub dies with the kill (FALSE: the old order)
    AbandonSettlesEarly,   \* FALSE in the code: stop leaves the claim `ending` until the group is gone (settle_gone)
    RestartKeepsClaim,     \* W adopt: an old instance's claim is not "ours": it stays until its process is gone
    ExternalChecksWarm,    \* X external.rs blocked_in: claim.external refuses (and marks the kill) while a warm process
                           \* has the conversation loaded
    RequireStub,           \* bin/kith: no stub program = refuse (KITH_REQUIRE_STUB); FALSE: an owner run goes unlocked
    OwnerNoAnswer,         \* K core_take NoAnswer: the core gives no answer, an owner's run goes on under the lock alone
    WrapperDies,           \* an interactive run's claim is on the `kith-sessions run` wrapper: it may die first
    \* --- scope switches
    GateFlips,             \* the gate (cutover.mayServe) may change between begins: Held
    HeldLaunch,            \* `kith --held`: the launcher gives its claim back, terminal.open takes it (M1-7)
    HandoverAtomic,        \* WHAT-IF: terminal.open takes over the launcher's claim in one step (no gap)
    LaunchIsInstant,       \* TRUE: claude is up the moment the launch passes its checks (no launch gap)
    ClaudeForks,           \* TRUE: claude may move to a new session id while it runs (/clear, /branch), which the
                           \* hook ties to the thread; FALSE: a run writes only the session id bin/kith claimed
    BotEnabled,            \* bots can `kith ask --thread` (api/botguests.rs ask)
    BotGate,               \* "live": S bot_gate (writer(), the code); "field": DESIGN, the summary's liveWriter
    \* --- what-ifs: fixes the code does NOT have (FALSE = code today)
    TakeoverAtomic,        \* recheck and signal as one step
    SyncLaneAtStart,       \* start_gate runs sync_one first (adopt a fork, import, forget) and judges the synced lane
    SendQueuedUnderLock    \* the send's entry is saved `queued` under the gate's lock (resolve's send_queued sees it)

VARIABLES
    TS,        \* per terminal: "fresh" "claimed" (resolve took the claim; hostBusy and live_pid not yet read)
               \* "launching" "idle" "busy" "dying" "gone"; "handing" (--held: the claim is given back, open has
               \* not taken it yet)
    sidOf,     \* per terminal: the session id it writes (0: none yet); its claim (cl) is on the id bin/kith claimed
    cl,        \* per session id: who holds its writer claim: a terminal, "app", "bot" (a host turn), "none".
               \* Files: <sid>.claim. CoreWriter: the core's claims row (one live row per conversation).
    regPid,    \* the terminal named by the registry file $KITH_STATE/live/terminal/<sid>.json ("none": no file)
    regSid,    \* the session id in that file
    hooked,    \* terminals whose SessionStart hook ran
    ghost,     \* gone terminals whose pid another process now has, sessions/<pid>.json still naming the session
    infoPid,   \* the host's copy of the registry (kv terminal.info.<thread>); "none": host-only thread
    infoSid,   \* the session id in that copy
    cur,       \* the Claude session id the last writer left (1, or 2 after a fork)
    lane,      \* the session id the host's lane resumes (kv lane.session...)
    gs,        \* sends past the gate, not yet queued (the api call between before_send and send_cmd)
    hq,        \* sends in the actor's queue
    hr,        \* the host turn in working_lanes (status Working): "none" "app" "bot"
    hp,        \* its phase: "none" "gated" (start_gate passed, no claim yet) "running" (claimed, prompt sent);
               \* CoreWriter also: "begun" (gate read ok) "claimed" (claim_next gave a claim) "locked" (the stub's
               \* take answered locked) "released" (lock given back, end not reported) "ending" (abandoned: lock
               \* given back, claim `ending` until the group is gone)
    hcl,       \* the session id the running host turn claimed (0: none)
    taking,    \* the thread is in Terminal.taking
    to,        \* take-over progress: "none" "wrote" "rechecked" "signalled" (not "none": its .takeover.claim is held)
    lw,        \* the summary's liveWriter field: "none" "terminal" "app" "bot"
    sig,       \* whom the last take-over signalled: "none" "own" "foreign"
    n,         \* sends made
    staleKv,   \* kv terminal.stale.<thread>: a terminal wrote since the host last loaded the session
    forgotten, \* the actor dropped its cached session (conn.loaded, thread_sessions): its next turn loads it again
    dirty,     \* a terminal wrote to the session after the actor last loaded it (the cache is out of date)
    \* --- CoreWriter only (constant, never moved, with CoreWriter = FALSE)
    lk,        \* per session id: who holds the stub's flock (U): a terminal's stub, "host" (a host stub: the current
               \* one while hp is begun/claimed/locked/running, an older host's after a restart), "none"
    eng,       \* the session id an engine process is WRITING to (0: none): a running turn, an abandoned one still
               \* alive, one left behind by a host restart, or an off-turn wake
    ld,        \* the session id a host's warm process has loaded (0: none): the warm_procs row
    ot,        \* eng is an off-turn wake: writing with no claim and no lock, ahead of the gate and the begin
    na,        \* terminals running on the lock alone (the core gave no answer; an owner run)
    gate,      \* cutover.mayServe claims: "ok" (ok:true) or "held" (anything else, the switch file there)
    again,     \* the turn that was released goes on as a new core turn (a move to the next account)
    pend,      \* the host restarted and `hostclaim.adopt` has not run yet
    ev         \* events that happened (for the reachability probes and the leak checks)

C == <<staleKv, forgotten, dirty>>
H == <<hr, hp, hcl>>
N1 == <<lk, eng, ot, na, gate, again, pend, ev>>
N == <<N1, ld>>

vars == <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig, n, C, N>>

Sids == {1, 2}

\* Q session.rs forget_loaded(): "A turn running there is left alone."
Forgets == ForgetsCache /\ hp /= "running"

Live == {"idle", "busy", "dying"}
Holding == {t \in Terminals : TS[t] \in Live}
Claiming == {t \in Terminals : TS[t] \in Live \cup {"launching", "claimed"}}

HostKind(c) == c \in {"app", "bot"}
\* ld: 0 none; s = a warm process has session s loaded; s + 2 = it has also run a turn (only such a process can wake
\* itself: a fresh one has nothing to wake with)
Loaded(s) == ld \in {s, s + 2}
\* The host's own claim rows, given back / ended (only the one of the running turn, as release_turn_claim does)
RelOwn(c) == IF hcl /= 0 /\ c[hcl] = hr THEN [c EXCEPT ![hcl] = "none"] ELSE c
ClrHost(c) == [s \in Sids |-> IF HostKind(c[s]) THEN "none" ELSE c[s]]
\* lock tables
LkTerm(t) == [s \in Sids |-> IF lk[s] = t THEN "none" ELSE lk[s]]
LkHost == [s \in Sids |-> IF lk[s] = "host" THEN "none" ELSE lk[s]]

\* R liveness(): pid runs, its session file names the session (and, with procStart, started when claimed).
VisOf(p) ==
    IF p = "none" THEN "closed"
    ELSE IF TS[p] = "busy" THEN "busy"
    ELSE IF TS[p] \in {"idle", "dying"} THEN "idle"
    ELSE IF TS[p] = "gone" /\ p \in ghost /\ ~ProcStartCheck THEN "idle"
    ELSE "closed"

\* R pid_runs(): kill(pid, 0).
Runs(p) == p /= "none" /\ (TS[p] \in Live \cup {"launching"} \/ (TS[p] = "gone" /\ p \in ghost))

\* C claim_live / Claim::is_live: a claim is live while its claimer runs and started when the claim says. A terminal's
\* claim is given back when its run ends (TermExit), a host turn's at the turn's end: so a claim here is live.
Free(s) == cl[s] = "none"

\* S writer(): under the thread lock, `taking`; then, for the lane's session and then the scanned copy's session: a
\* claim (claim::holder: not a host turn's own "app" claim; a "bot" claim is Writer::Bot), or a live registry file of
\* that session (registry::read_one + liveness); last, the scanned copy's own liveness. (CoreWriter: claim::holder
\* reads the core, P8.)
RegLive(s) == regPid /= "none" /\ regSid = s /\ VisOf(regPid) /= "closed"
WriterOf(s) == IF cl[s] \in Terminals THEN "terminal"
               ELSE IF cl[s] = "bot" THEN "bot"
               ELSE IF RegLive(s) THEN "terminal" ELSE "none"
WriterAt(l, ip, is) ==
    IF taking /\ TakingBlocksSend THEN "terminal"
    ELSE IF WriterOf(l) /= "none" THEN WriterOf(l)
    ELSE IF ip /= "none" /\ WriterOf(is) /= "none" THEN WriterOf(is)
    ELSE IF ip /= "none" /\ VisOf(ip) /= "closed" THEN "terminal"
    ELSE "none"
Writer == WriterAt(lane, infoPid, infoSid)

\* S sync_one(): `mapped` adopts the registry's session (a fork) as the lane's, and the scanned copy is refreshed.
SyncLane == IF regPid /= "none" THEN regSid ELSE lane
SyncInfoPid == IF regPid /= "none" THEN regPid ELSE infoPid
SyncInfoSid == IF regPid /= "none" THEN regSid ELSE infoSid
SyncedWriter == WriterAt(SyncLane, SyncInfoPid, SyncInfoSid)

TypeOK ==
    /\ TS \in [Terminals -> {"fresh", "claimed", "launching", "idle", "busy", "dying", "gone", "handing"}]
    /\ sidOf \in [Terminals -> {0} \cup Sids] /\ cl \in [Sids -> Terminals \cup {"none", "app", "bot"}]
    /\ regPid \in Terminals \cup {"none"} /\ infoPid \in Terminals \cup {"none"}
    /\ hr \in {"none", "app", "bot"}
    /\ hp \in {"none", "gated", "begun", "claimed", "locked", "running", "released", "ending"} /\ hcl \in {0} \cup Sids
    /\ to \in {"none", "wrote", "rechecked", "signalled"}
    /\ lw \in {"none", "terminal", "app", "bot"} /\ sig \in {"none", "own", "foreign"}
    /\ Len(gs) + Len(hq) <= MaxSends /\ n <= MaxSends
    /\ lk \in [Sids -> Terminals \cup {"none", "host"}] /\ eng \in {0} \cup Sids /\ ld \in {0, 1, 2, 3, 4}
    /\ ot \in BOOLEAN /\ na \subseteq Terminals /\ gate \in {"ok", "held"} /\ again \in BOOLEAN /\ pend \in BOOLEAN
    /\ ev \subseteq {"held", "busy", "settled", "deadrefused", "died", "killed", "failover", "restart", "restartlocked",
                     "offturn", "lockout", "lockalone", "lost", "abandoned"}

-----------------------------------------------------------------------------
Init ==
    /\ n = 0 /\ gs = <<>> /\ hq = <<>> /\ hr = "none" /\ hp = "none" /\ hcl = 0 /\ taking = FALSE /\ to = "none"
    /\ sig = "none" /\ ghost = {} /\ cur = 1 /\ lane = 1 /\ regSid = 1 /\ infoSid = 1
    /\ forgotten = FALSE /\ dirty = FALSE
    /\ eng = 0 /\ ld = 0 /\ ot = FALSE /\ na = {} /\ gate = "ok" /\ again = FALSE /\ pend = FALSE /\ ev = {}
    /\ \/ \* a thread made in the app, or one older than the terminal feature: the host knows no terminal
          /\ TS = [t \in Terminals |-> "fresh"] /\ sidOf = [t \in Terminals |-> 0] /\ cl = [s \in Sids |-> "none"]
          /\ regPid = "none" /\ infoPid = "none" /\ lw = "none" /\ hooked = {} /\ staleKv = FALSE
          /\ lk = [s \in Sids |-> "none"]
       \/ \* a thread born in a terminal `kith <agent>` that is open now, hooked and already scanned; the run claimed
          \* its session id before claude started (FreshClaims)
          /\ StartTerminalBorn
          /\ TS = [t \in Terminals |-> IF t = "t1" THEN "idle" ELSE "fresh"]
          /\ sidOf = [t \in Terminals |-> IF t = "t1" THEN 1 ELSE 0]
          /\ cl = [s \in Sids |-> IF s = 1 /\ FreshClaims THEN "t1" ELSE "none"]
          /\ lk = [s \in Sids |-> IF s = 1 /\ CoreWriter /\ TermTakesLock THEN "t1" ELSE "none"]
          /\ regPid = "t1" /\ infoPid = "t1" /\ lw = "terminal" /\ hooked = {"t1"}
          /\ staleKv = TRUE   \* S create_thread: a live terminal thread starts stale

\* ---- the app / bot side ------------------------------------------------

\* T/bots.rs `send` -> S before_send_blocking(), under the thread lock: refused (409) while writer() names anyone;
\* else sync_one: `mapped` adopts a fork of the lane's session (the lane moves to it), the scanned copy is refreshed and
\* the new lines imported (forget_loaded when any came), then a `stale` mark is deleted with forget_loaded.
AppGate ==
    /\ n < MaxSends /\ ~(SendChecksLive /\ Writer /= "none")
    /\ gs' = Append(gs, "app") /\ n' = n + 1
    /\ LET adopted == regPid /= "none" /\ regSid /= lane
           synced == regPid /= "none" /\ (adopted \/ regSid = lane)
       IN /\ lane' = IF adopted THEN regSid ELSE lane
          /\ infoPid' = IF synced THEN regPid ELSE infoPid
          /\ infoSid' = IF synced THEN regSid ELSE infoSid
          /\ staleKv' = FALSE
          /\ forgotten' = (forgotten \/ ((adopted \/ staleKv \/ (ForgetOnImport /\ synced /\ dirty)) /\ Forgets))
    /\ UNCHANGED <<N, TS, sidOf, cl, regPid, regSid, hooked, ghost, cur, hq, H, taking, to, lw, sig, dirty>>

\* api/botguests.rs ask -> S bot_gate(): writer() under the thread lock; a bot's own claim (its turn) lets it queue.
\* DESIGN "field": a gate reading the summary's liveWriter.
BotRefuses == IF BotGate = "field" THEN lw \in {"terminal", "app"} ELSE Writer = "terminal"
BotGate_ ==
    /\ BotEnabled /\ n < MaxSends /\ ~BotRefuses
    /\ gs' = Append(gs, "bot") /\ n' = n + 1
    /\ lw' = IF lw = "none" THEN "bot" ELSE lw
    /\ UNCHANGED <<N, TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, hq, H, taking, to, sig, C>>

\* api/bots.rs send: the user entry saved `queued`, then hub send_cmd(Cmd::Send) -> Q queue.rs on_cmd pushes
\* Queued::User. (Merged: resolve's send_queued sees the entry a moment before the queue does.)
Enqueue ==
    /\ gs /= <<>>
    /\ hq' = Append(hq, Head(gs)) /\ gs' = Tail(gs)
    /\ UNCHANGED <<N, TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, H, taking, to, lw, sig, n, C>>

\* Q queue.rs next_in_queue -> turn.rs start_turn_inner -> S start_gate(), under the thread lock: refused while
\* writer() is a terminal (the message fails with a notice); else `begin` marks the turn Working (working_lanes).
\* The turn then loads its session (a forgotten cache is loaded again). SyncLaneAtStart is a what-if.
\* CoreWriter: the process is spawned and registered (W ensure_hq_session, warm_procs, `ld`) in the same step as the
\* gate; a begin only after `hostclaim.adopt` ran (host start). Not modelled: the registration window of a FRESH
\* process (it has nothing to wake itself with).
HostStart ==
    /\ hr = "none" /\ hq /= <<>> /\ ~pend
    /\ hq' = Tail(hq)
    /\ IF StartGateChecks /\ (IF SyncLaneAtStart THEN SyncedWriter ELSE Writer) = "terminal"
       THEN UNCHANGED <<H, lane, infoPid, infoSid, forgotten, dirty, ld>>
       ELSE /\ hr' = Head(hq) /\ hp' = "gated" /\ UNCHANGED hcl
            /\ ld' = (IF CoreWriter THEN (IF ld = lane + 2 THEN ld ELSE lane) ELSE ld)
            /\ IF SyncLaneAtStart
               THEN lane' = SyncLane /\ infoPid' = SyncInfoPid /\ infoSid' = SyncInfoSid
               ELSE UNCHANGED <<lane, infoPid, infoSid>>
            \* a forgotten cache means this turn loads the session again, and so sees what the terminal wrote
            /\ IF forgotten \/ (SyncLaneAtStart /\ ForgetOnImport /\ Forgets /\ regPid /= "none" /\ dirty)
               THEN forgotten' = FALSE /\ dirty' = FALSE ELSE UNCHANGED <<forgotten, dirty>>
    /\ UNCHANGED <<N1, TS, sidOf, cl, regPid, regSid, hooked, ghost, cur, gs, taking, to, lw, sig, n, staleKv>>

\* Q offturn.rs adopt_off_turn (the agent's own session/update with no turn running, or its send_file / send_message):
\* the same start_gate() as HostStart under the thread lock, then claim_for_turn (HostClaim) and HostEnd; only the
\* queue is skipped. Refused (the update is dropped, a send gets an error and a notice) while a terminal holds the
\* thread: nothing changes, so the refusal is a stutter here. It never skips the gate, which keeps the new path from
\* being a way to write next to a terminal.
HostWake ==
    /\ OffTurnWakes /\ hr = "none" /\ hp = "none" /\ ~pend
    /\ ~(StartGateChecks /\ (IF SyncLaneAtStart THEN SyncedWriter ELSE Writer) = "terminal")
    /\ hr' = "app" /\ hp' = "gated" /\ UNCHANGED hcl
    /\ ld' = (IF CoreWriter THEN (IF ld = lane + 2 THEN ld ELSE lane) ELSE ld)
    /\ IF SyncLaneAtStart
       THEN lane' = SyncLane /\ infoPid' = SyncInfoPid /\ infoSid' = SyncInfoSid
       ELSE UNCHANGED <<lane, infoPid, infoSid>>
    /\ IF forgotten \/ (SyncLaneAtStart /\ ForgetOnImport /\ Forgets /\ regPid /= "none" /\ dirty)
       THEN forgotten' = FALSE /\ dirty' = FALSE ELSE UNCHANGED <<forgotten, dirty>>
    /\ UNCHANGED <<N1, TS, sidOf, cl, regPid, regSid, hooked, ghost, cur, gs, hq, taking, to, lw, sig, n, staleKv>>

\* Q turn.rs claim_for_turn (after the session is loaded, before session/prompt): take_as(kind App, or Bot for a bot's
\* conversation) for the agent process. Held by another (a terminal, a one-shot) -> HeldByWriter, the turn fails.
\* The claim FILES (CoreWriter = FALSE).
HostClaim ==
    /\ ~CoreWriter /\ hp = "gated"
    /\ IF Free(lane)
       THEN /\ cl' = [cl EXCEPT ![lane] = hr] /\ hp' = "running" /\ hcl' = lane /\ UNCHANGED hr
       ELSE IF HostStartCAS
            THEN /\ hr' = "none" /\ hp' = "none" /\ UNCHANGED <<cl, hcl>>
            ELSE /\ hp' = "running" /\ hcl' = lane /\ UNCHANGED <<cl, hr>>   \* broken: runs without the claim
    /\ UNCHANGED <<N, TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, taking, to, lw, sig, n, C>>

\* The writing turn's session is the lane's, which the last writer left.
HostWrites == cur' = lane

\* Q turn.rs finish (done, failed or Stop): the turn ends, release_turn_claim gives the claim back. (Files.)
HostEnd ==
    /\ ~CoreWriter /\ hp /= "none"
    /\ hr' = "none" /\ hp' = "none" /\ hcl' = 0
    /\ cl' = IF hcl /= 0 /\ cl[hcl] = hr THEN [cl EXCEPT ![hcl] = "none"] ELSE cl
    /\ IF hp = "running" THEN HostWrites ELSE UNCHANGED cur
    /\ UNCHANGED <<N, TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, lane, gs, hq, taking, to, lw, sig, n, C>>

\* The host process restarts: memory is lost (runtime, queue, taking); kv (info, lane, liveWriter) stays. The agent
\* process ends with it, and with it its turn claim; the host's take-over claim is stale (its pid is gone). (Files.)
HostRestart ==
    /\ ~CoreWriter
    /\ hr' = "none" /\ hp' = "none" /\ hcl' = 0 /\ hq' = <<>> /\ gs' = <<>> /\ taking' = FALSE /\ to' = "none"
    /\ cl' = [s \in Sids |-> IF cl[s] \in {"app", "bot"} THEN "none" ELSE cl[s]]
    /\ forgotten' = FALSE /\ dirty' = FALSE   \* no actor, no cache: the next turn loads the session
    /\ UNCHANGED <<N, TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, lw, sig, n, staleKv>>

\* ---- the host turn in Writer mode (CoreWriter) --------------------------
\* W core_writer.rs begin / finish / abandon / adopt, agent/bot/writer*.rs. The design's steps (1.3):
\* process (HostStart/HostWake), gate, [settle], claim_next, take, redeem, [sync, prompt], end.

\* W begin: "The gate is read first, at every begin: only ok:true is Writer, and in Held the core is asked nothing else."
\* Held: the turn waits (Begin::NotWriter), no hostclaim.* call; the item stays queued, so here it just ends.
\* GateRead = FALSE: no gate read (broken).
HostGateRead ==
    /\ CoreWriter /\ hp = "gated" /\ Loaded(lane)
    /\ IF GateRead /\ gate /= "ok"
       THEN /\ hr' = "none" /\ hp' = "none" /\ ev' = ev \cup {"held"}
       ELSE /\ hp' = "begun" /\ UNCHANGED <<hr, ev>>
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, hcl, taking, to,
                   lw, sig, n, C, lk, eng, ot, na, gate, again, pend, ld>>

\* W begin: a stopped turn claim (`ending`) whose stub is gone holds the conversation until someone settles it: the core
\* ends it (hostclaim.settle) and this begin goes on to claim. Any host's: the check names no instance (core_writer.rs
\* begin, `stub_gone`). Here the dead claim is a host claim with no engine alive on its session.
Dead == HostKind(cl[lane]) /\ eng /= lane
HostSettleOwn ==
    /\ CoreWriter /\ SettlesOwnEnding /\ hp = "begun" /\ Dead
    /\ cl' = [cl EXCEPT ![lane] = "none"] /\ ev' = ev \cup {"settled"}
    /\ UNCHANGED <<TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig,
                   n, C, lk, eng, ot, na, gate, again, pend, ld>>

\* W attempt step 3, hostclaim.turn = X claims.rs claim_next, the only way to claim a turn: a live claim of anyone
\* answers `held` (the turn stays queued, or parks `terminal_live`: here it ends and a later send begins again).
\* ClaimNextChecksHeld = FALSE (broken): the claim replaces whatever is there. The process must still be the one loaded.
HostClaimNext ==
    /\ CoreWriter /\ hp = "begun" /\ Loaded(lane) /\ ~(SettlesOwnEnding /\ Dead)
    /\ IF Free(lane) \/ ~ClaimNextChecksHeld
       THEN /\ cl' = [cl EXCEPT ![lane] = hr] /\ hp' = "claimed" /\ hcl' = lane /\ UNCHANGED <<hr, ev>>
       ELSE /\ hr' = "none" /\ hp' = "none" /\ UNCHANGED <<cl, hcl>>
            /\ ev' = (IF Dead THEN ev \cup {"deadrefused"} ELSE ev)
    /\ UNCHANGED <<TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, taking, to, lw, sig, n,
                   C, lk, eng, ot, na, gate, again, pend, ld>>

\* W attempt step 5, the stub's `take <key> <claim>` (U handle): non-blocking on the same lock file every stub uses;
\* held by anyone (a terminal's stub, an older host's) answers `busy`: the claim ends `busy` (hostclaim.report) and the
\* turn parks `lock_busy`. HostTakesLock = FALSE (broken): the host does not take the lock.
HostTake ==
    /\ CoreWriter /\ hp = "claimed" /\ Loaded(hcl)   \* the current process is still registered (else HostGiveUp)
    /\ IF HostTakesLock /\ lk[hcl] /= "none"
       THEN /\ cl' = RelOwn(cl) /\ hr' = "none" /\ hp' = "none" /\ hcl' = 0 /\ ev' = ev \cup {"busy"} /\ UNCHANGED lk
       ELSE /\ hp' = "locked" /\ lk' = (IF HostTakesLock THEN [lk EXCEPT ![hcl] = "host"] ELSE lk)
            /\ UNCHANGED <<cl, hr, hcl, ev>>
    /\ UNCHANGED <<TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, taking, to, lw, sig, n,
                   C, eng, ot, na, gate, again, pend, ld>>

\* W redeem (hostclaim.redeem on the stub's pid), then the transcript sync under the lock and session/prompt: the engine
\* writes. The turn it was waiting for is the off-turn wake's, if there was one (ot): it is Running now.
HostRedeem ==
    /\ CoreWriter /\ hp = "locked" /\ Loaded(hcl) /\ (HostTakesLock => lk[hcl] = "host")   \* the stub is still there (else HostGiveUp)
    /\ hp' = "running" /\ eng' = hcl /\ ot' = FALSE
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, hr, hcl, taking, to,
                   lw, sig, n, C, lk, na, gate, again, pend, ev, ld>>

\* W finish End::Done / OutOfUsage / Left: the lock is given back first (`release`), the engine is not writing; the end
\* is reported next (HostReport). again: OutOfUsage / Left move on to the next account / session as a NEW core turn.
HostFinish ==
    /\ CoreWriter /\ hp = "running" /\ eng = hcl
    /\ hp' = "released" /\ eng' = 0 /\ lk' = (IF lk[hcl] = "host" THEN [lk EXCEPT ![hcl] = "none"] ELSE lk)
    /\ ld' = hcl + 2
    /\ \E f \in BOOLEAN : again' = f
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, hr, hcl, taking, to,
                   lw, sig, n, C, ot, na, gate, pend, ev>>

\* W retry_end / report: `done` (or `exit` + out_of_usage): the claim ends. A move to the next account cancels the
\* requeued core turn and the item begins again (writer_turn.rs acc.turn = None): hp gated, a new claim and lock.
\* FailoverReclaims = FALSE (broken): the host's own retry runs on, with no new claim and no lock.
HostReport ==
    /\ CoreWriter /\ hp = "released"
    /\ cl' = RelOwn(cl)
    /\ HostWrites
    /\ again' = FALSE
    /\ IF again
       THEN /\ ev' = ev \cup {"failover"} /\ UNCHANGED hr
            /\ (IF FailoverReclaims
                THEN hp' = "gated" /\ hcl' = 0 /\ UNCHANGED eng
                ELSE hp' = "running" /\ eng' = hcl /\ UNCHANGED hcl)
       ELSE /\ hr' = "none" /\ hp' = "none" /\ hcl' = 0 /\ UNCHANGED <<eng, ev>>
    /\ UNCHANGED <<TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, lane, gs, hq, taking, to, lw, sig, n, C,
                   lk, ot, na, gate, pend, ld>>

\* W abandon: a begun turn given up before it ran (the item was cancelled between begin and the prompt): the lock back,
\* the claim stopped. The engine has not written. (Also a redeem the core refused.)
HostGiveUp ==
    /\ CoreWriter /\ hp \in {"claimed", "locked"}
    /\ cl' = RelOwn(cl) /\ lk' = (IF lk[hcl] = "host" THEN [lk EXCEPT ![hcl] = "none"] ELSE lk)
    /\ hr' = "none" /\ hp' = "none" /\ hcl' = 0
    /\ UNCHANGED <<TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, taking, to, lw, sig, n, C,
                   eng, ot, na, gate, again, pend, ev, ld>>

\* writer_turn.rs Rel::Abandon on a running turn: core.abandon = `hostclaim.stop` (the claim stays `ending` until the process
\* is gone: the core ends a stopped running claim only then, X end_gone), then killpg SIGKILL (HostKill), then settle_gone.
\* The lock is NOT given back: it goes when the stub dies (core_writer.rs abandon, warm_stub.rs), so both layers hold until
\* the engine is gone.
\* AbandonKeepsLock = FALSE (broken: the old order): the lock is given back first, the engine is held by its claim alone.
\* AbandonSettlesEarly = TRUE (broken): the claim ends at the stop.
HostAbandon ==
    /\ CoreWriter /\ hp = "running" /\ eng = hcl
    /\ lk' = (IF ~AbandonKeepsLock /\ lk[hcl] = "host" THEN [lk EXCEPT ![hcl] = "none"] ELSE lk)
    /\ hp' = "ending"
    /\ cl' = (IF AbandonSettlesEarly THEN RelOwn(cl) ELSE cl)
    /\ ev' = ev \cup {"abandoned"}
    /\ UNCHANGED <<TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, hr, hcl, taking, to, lw,
                   sig, n, C, eng, ot, na, gate, again, pend, ld>>

\* stub.kill_group() (SIGKILL on the process group): the engine is gone.
HostKill ==
    /\ CoreWriter /\ hp = "ending" /\ eng = hcl
    /\ eng' = 0 /\ ld' = 0
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw,
                   sig, n, C, lk, ot, na, gate, again, pend, ev>>

\* W settle_gone (after an abandon, and an End::Room): wait for the group to go (the engine and the stub, so its lock too),
\* then hostclaim.settle + proc_gone: the claim ends only now. Not before the process is gone.
HostSettle ==
    /\ CoreWriter /\ hp = "ending" /\ eng /= hcl /\ lk[hcl] /= "host"
    /\ cl' = RelOwn(cl) /\ HostWrites
    /\ hr' = "none" /\ hp' = "none" /\ hcl' = 0
    /\ UNCHANGED <<TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, lane, gs, hq, taking, to, lw, sig, n, C,
                   lk, eng, ot, na, gate, again, pend, ev, ld>>

\* W finish End::Killed (the 15 s kill, mod.rs): the end is reported at once (`exit`), so the core ends the claim, and
\* the group gets SIGKILL: dying, not writing, but the stub keeps its lock until it exits (StubExit). The end is NOT
\* held back for settle_gone (that is End::Room and the abandon).
HostKilled ==
    /\ CoreWriter /\ hp = "running" /\ eng = hcl
    /\ cl' = RelOwn(cl) /\ HostWrites
    /\ eng' = 0 /\ ld' = 0 /\ ev' = ev \cup {"killed"}
    /\ hr' = "none" /\ hp' = "none" /\ hcl' = 0
    /\ UNCHANGED <<TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, lane, gs, hq, taking, to, lw, sig, n, C,
                   lk, ot, na, gate, again, pend>>

\* The engine process ends on its own (a crash, a quit, the usage limit killing it): not writing any more, and the
\* stub's flock goes with the stub. The claim row stays: the core never ends a host turn claim for a gone pid by
\* itself (X end_gone skips them), the host reports it (HostDied) or the next begin settles it.
EngineDied ==
    /\ CoreWriter /\ eng /= 0
    /\ eng' = 0 /\ ot' = FALSE /\ ld' = 0 /\ lk' = LkHost
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw,
                   sig, n, C, na, gate, again, pend, ev>>

\* W finish End::Died: the process is gone under its turn: `exit` is reported (the claim ends, the core puts the turn
\* back) and the host cancels the requeued core turn; the item is rerun as a new send, or not (the choice here).
\* DiedReports = FALSE (broken): the claim is left.
HostDied ==
    /\ CoreWriter /\ hp = "running" /\ eng = 0
    /\ cl' = (IF DiedReports THEN RelOwn(cl) ELSE cl) /\ HostWrites
    /\ \E rr \in BOOLEAN :   \* (an off-turn wake has no send to rerun: only a send not queued any more)
          /\ rr => Len(gs) + Len(hq) < n
          /\ hq' = (IF rr THEN Append(hq, hr) ELSE hq)
    /\ hr' = "none" /\ hp' = "none" /\ hcl' = 0 /\ ev' = ev \cup {"died"}
    /\ UNCHANGED <<TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, lane, gs, taking, to, lw, sig, n, C,
                   lk, eng, ot, na, gate, again, pend, ld>>

\* The warm process ends while nothing runs in it: a terminal's idle-kill mark (X external.rs blocked_in answers
\* `waiting` and sets kill_at; kill_resend signals it), a revoke, the actor retiring.
WarmKilled ==
    /\ CoreWriter /\ ld /= 0 /\ hp = "none" /\ eng = 0
    /\ ld' = 0 /\ lk' = LkHost
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw,
                   sig, n, C, eng, ot, na, gate, again, pend, ev>>

\* The stub of a process that never got to write exits (an older host's, killed by adopt's mark): its lock goes.
StubExit ==
    /\ CoreWriter /\ \E s \in Sids : lk[s] = "host" /\ eng /= s /\ hp \notin {"locked", "running"}
    /\ lk' = LkHost   \* (a stub of a process that never wrote: its registration stays until the process is killed)
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw,
                   sig, n, C, eng, ot, na, gate, again, pend, ev, ld>>

\* Q offturn.rs: an agent that woke itself (a finished background task) writes before the gate and the begin run: no
\* claim, no lock (released at the end of the last turn), only the warm-process check keeps a terminal out. Anything
\* short of Running kills its group (OffTurnKilled).
OffTurnWrite ==
    /\ CoreWriter /\ OffTurnWakes /\ ld \in {3, 4} /\ eng = 0 /\ hp \in {"none", "gated", "begun"} /\ ~pend   \* it has run a turn
    /\ eng' = ld - 2 /\ ot' = TRUE /\ ev' = ev \cup {"offturn"}
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw,
                   sig, n, C, lk, na, gate, again, pend, ld>>

OffTurnKilled ==
    /\ CoreWriter /\ ot /\ hp = "none"   \* offturn.rs: the kill follows a begin that did not return Running
    /\ eng' = 0 /\ ot' = FALSE /\ ld' = 0
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw,
                   sig, n, C, lk, na, gate, again, pend, ev>>

\* The host process restarts (Writer mode): its memory is lost, but the stub outlives it and keeps the engine and its
\* lock; the claim row stays. A claim not redeemed has no process. RestartKeepsClaim = FALSE (broken): the old
\* instance's claim reads as "ours" and ends (HOST_INSTANCE as one constant, core_claims.rs:269-281).
HostRestartCore ==
    /\ CoreWriter
    /\ hr' = "none" /\ hp' = "none" /\ hcl' = 0 /\ hq' = <<>> /\ gs' = <<>> /\ taking' = FALSE /\ to' = "none"
    /\ cl' = (IF RestartKeepsClaim THEN cl ELSE ClrHost(cl))
    /\ forgotten' = FALSE /\ dirty' = FALSE /\ again' = FALSE /\ pend' = TRUE
    /\ ev' = ev \cup {"restart"} \cup (IF hp = "locked" THEN {"restartlocked"} ELSE {})
    /\ UNCHANGED <<TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, lw, sig, n, staleKv, lk, eng,
                   ot, na, gate, ld>>

\* W adopt (hostclaim.adopt, first thing at host start): ends every claim never redeemed at once (and the requeued turn),
\* marks every process of another instance for a kill; a claim with a process stays the writer until it is gone.
HostAdopt ==
    /\ CoreWriter /\ pend
    /\ pend' = FALSE
    /\ cl' = [s \in Sids |-> IF HostKind(cl[s]) /\ eng /= s THEN "none" ELSE cl[s]]
    /\ ld' = (IF eng = 0 THEN 0 ELSE ld)   \* an idle process of the old instance is marked and goes; a new host never reuses it
    /\ UNCHANGED <<TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig,
                   n, C, lk, eng, ot, na, gate, again, ev>>

\* The marked engine is killed (kill_resend), its stub exits and the lock goes. The claim stays, `ending`, until the
\* host (a begin's settle, or its tick) lets the core end it.
OrphanGone ==
    /\ CoreWriter /\ ~pend /\ hp = "none" /\ eng /= 0 /\ ~ot
    /\ eng' = 0 /\ ld' = 0 /\ lk' = LkHost
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw,
                   sig, n, C, ot, na, gate, again, pend, ev>>

\* cutover.mayServe {module: "claims"} answers ok:true, or anything else (a warning, a refusal, no core) with the switch
\* file there: Held. Read again at every begin.
GateFlip ==
    /\ CoreWriter /\ GateFlips
    /\ gate' = (IF gate = "ok" THEN "held" ELSE "ok")
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw,
                   sig, n, C, lk, eng, ot, na, again, pend, ev, ld>>

\* ---- the terminal side -------------------------------------------------

Up == IF LaunchIsInstant THEN "idle" ELSE "launching"

\* The claim a taker asks of the core (X external.rs claim.external / claim_terminal, K core_take): refused while a live
\* claim of anyone holds the conversation, while a take-over is queued, and, in CoreWriter, while a warm process has the
\* conversation loaded (the core marks it for a kill and answers `waiting`; the run retries). Files: no warm check.
WarmOK(s) == ~(CoreWriter /\ ExternalChecksWarm /\ Loaded(s))

\* K kith-sessions resolve() (`kith <agent> --resume|-r|-c`, also a one-shot with a task): the host names the session
\* (the lane's), then claim(): take_claim (exclusive; a live claim of anyone refuses, a one-shot waits a little), and
\* a live take-over claim refuses. ResumeClaims = FALSE: no claim (a broken variant). CoreWriter: claim.external, and the
\* owner's run goes on without one when the core gives no answer (OwnerNoAnswer, the lock alone guards it).
TermClaim(t) ==
    /\ TS[t] = "fresh" /\ to = "none"
    /\ \/ /\ ResumeClaims => (Free(lane) /\ WarmOK(lane))
          /\ TS' = [TS EXCEPT ![t] = "claimed"] /\ sidOf' = [sidOf EXCEPT ![t] = lane]
          /\ cl' = IF ResumeClaims /\ TermRegistersClaim THEN [cl EXCEPT ![lane] = t] ELSE cl
          /\ UNCHANGED <<na, ev>>
       \/ /\ CoreWriter /\ OwnerNoAnswer /\ TermTakesLock /\ gate = "held"   \* the core is not answering
          /\ TS' = [TS EXCEPT ![t] = "claimed"] /\ sidOf' = [sidOf EXCEPT ![t] = lane]
          /\ na' = na \cup {t} /\ ev' = ev \cup {"lockalone"} /\ UNCHANGED cl
    /\ UNCHANGED <<lk, eng, ot, gate, again, pend, ld, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq,
                   H, taking, to, lw, sig, n, C>>

\* K resolve(), with the claim held: resolveThread again (S resolve: hostBusy, read under the thread lock, = a turn in
\* working_lanes, or a queued send, or `taking`), then live_pid of the registry file. A refusal dies and gives the
\* claim back (release_claims); else bin/kith goes on to start claude.
HostBusy == hr /= "none" \/ (HostBusyCountsQueued /\ (hq /= <<>> \/ taking)) \/ (SendQueuedUnderLock /\ gs /= <<>>)
TermResolve(t) ==
    /\ TS[t] = "claimed"
    /\ IF (ResumeChecksHostBusy /\ HostBusy) \/ (ResumeChecksOpen /\ regPid /= "none" /\ TS[regPid] \in Live)
       THEN /\ TS' = [TS EXCEPT ![t] = "gone"]
            /\ cl' = IF cl[sidOf[t]] = t THEN [cl EXCEPT ![sidOf[t]] = "none"] ELSE cl
            /\ na' = na \ {t}
       ELSE /\ TS' = [TS EXCEPT ![t] = Up] /\ UNCHANGED <<cl, na>>
    /\ UNCHANGED <<sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig, n, C,
                   lk, eng, ot, gate, again, pend, ev, ld>>

\* K bin/kith, a Claude start resolve did not claim: a run that continues the thread's session named by its caller
\* (KITH_RESUME: a channel, `kith ask -c`; the bridge's --resume in KITH_CLAUDE_ARGS). `kith-sessions terminal`
\* (interactive, refused at once) or `kith-sessions claim` (one-shot, waits a little) takes the claim for the launcher's
\* pid (exec / `run` keep it) before claude starts; take_claim reads only claims (a live take-over claim refuses), not
\* the registry, and the host is not asked. FreshClaims = FALSE: started without a claim (before #324; fresh-no-claim).
\* A fresh id or a fork at launch is a new session no thread names yet: it joins one only through the hook (TermHook).
TermLaunch(t) ==
    /\ TS[t] = "fresh"
    /\ \/ /\ FreshClaims => (Free(cur) /\ to = "none" /\ WarmOK(cur))
          /\ cl' = IF FreshClaims /\ TermRegistersClaim THEN [cl EXCEPT ![cur] = t] ELSE cl
          /\ UNCHANGED <<na, ev>>
       \/ /\ CoreWriter /\ OwnerNoAnswer /\ TermTakesLock /\ gate = "held"   \* kith-sessions claim / terminal, core_take NoAnswer
          /\ na' = na \cup {t} /\ ev' = ev \cup {"lockalone"} /\ UNCHANGED cl
    /\ sidOf' = [sidOf EXCEPT ![t] = cur]
    /\ TS' = [TS EXCEPT ![t] = Up]
    /\ UNCHANGED <<lk, eng, ot, gate, again, pend, ld, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig, n, C>>

\* bin/kith -> kith-sessions run (or exec) -> kith-stub -> claude starts and writes ~/.claude/sessions/<pid>.json: the
\* session is open. CoreWriter: the stub takes the writer lock first (U take(): non-blocking on the same file every stub
\* uses); busy -> it exits 75 and the claim is given back (run's release, or the pid going, one-shot). RequireStub =
\* FALSE: no stub program on the Mac, the run is unlocked.
LockWanted == CoreWriter /\ TermTakesLock
TermUp(t) ==
    /\ TS[t] = "launching"
    /\ \/ \* busy: exit 75, the claim given back
          /\ LockWanted /\ (RequireStub \/ t \in na) /\ lk[sidOf[t]] /= "none"
          /\ TS' = [TS EXCEPT ![t] = "gone"]
          /\ cl' = [s \in Sids |-> IF cl[s] = t THEN "none" ELSE cl[s]]
          /\ ev' = ev \cup {"lockout"} /\ na' = na \ {t} /\ UNCHANGED lk
       \/ \* locked
          /\ LockWanted /\ lk[sidOf[t]] = "none"
          /\ TS' = [TS EXCEPT ![t] = "idle"] /\ lk' = [lk EXCEPT ![sidOf[t]] = t] /\ UNCHANGED <<cl, ev, na>>
       \/ \* no lock: the claim files, or no stub program on this Mac (RequireStub = FALSE; never an owner on the lock alone)
          /\ ~LockWanted \/ (~RequireStub /\ t \notin na)
          /\ TS' = [TS EXCEPT ![t] = "idle"] /\ UNCHANGED <<cl, ev, na, lk>>
    /\ UNCHANGED <<sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig, n, C,
                   eng, ot, gate, again, pend, ld>>

\* K record(): the SessionStart hook writes the registry file (write_atomic, one file per session id: a later
\* terminal replaces the pid in it). The session id is the one bin/kith claimed (--session-id, or the --resume id), or
\* a new one claude moved to (a fork: /clear, /branch; record() ties it to the thread by KITH_RESUMED_FROM, and the
\* claim stays on the id bin/kith claimed).
TermHook(t) ==
    /\ TS[t] \in {"idle", "busy"} /\ t \notin hooked
    /\ \E s \in IF ClaudeForks THEN {sidOf[t], 2} ELSE {sidOf[t]} :
          /\ sidOf' = [sidOf EXCEPT ![t] = s]
          /\ cur' = s /\ regSid' = s
    /\ hooked' = hooked \cup {t} /\ regPid' = t
    /\ UNCHANGED <<N, TS, cl, ghost, infoPid, infoSid, lane, gs, hq, H, taking, to, lw, sig, n, C>>

\* The user types a prompt / the turn ends (claude's own status, read by R liveness: idle vs anything else).
TermPrompt(t) ==
    /\ TS[t] = "idle" /\ t \in hooked
    /\ TS' = [TS EXCEPT ![t] = "busy"] /\ dirty' = TRUE   \* the session now holds what the host's cache lacks
    /\ cur' = sidOf[t]
    /\ UNCHANGED <<N, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, lane, gs, hq, H, taking, to, lw, sig, n, staleKv, forgotten>>

TermDone(t) ==
    /\ TS[t] = "busy"
    /\ TS' = [TS EXCEPT ![t] = "idle"]
    /\ UNCHANGED <<N, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig, n, C>>

\* The user quits (or it crashes, or a refused launch dies). A crash can leave sessions/<pid>.json behind with the pid
\* reused (ghost). K run gives the claim back after claude exits; an exec'd one-shot's claim is stale with its pid. The
\* stub's flock goes with the process.
TermExit(t) ==
    /\ TS[t] \in Live \cup {"launching", "claimed"}
    /\ TS' = [TS EXCEPT ![t] = "gone"]
    /\ cl' = [s \in Sids |-> IF cl[s] = t THEN "none" ELSE cl[s]]
    /\ lk' = LkTerm(t) /\ na' = na \ {t}
    /\ \E g \in BOOLEAN : ghost' = IF g /\ TS[t] /= "claimed" THEN ghost \cup {t} ELSE ghost
    /\ UNCHANGED <<sidOf, regPid, regSid, hooked, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig, n, C,
                   eng, ot, gate, again, pend, ev, ld>>

\* An interactive run's claim is on the `kith-sessions run` wrapper's pid, with the stub and claude as its children: the
\* wrapper dies first, the claim ends, and claude goes on holding the lock (inferred from the code, not run).
TermWrapperDies(t) ==
    /\ CoreWriter /\ WrapperDies /\ TS[t] \in Live /\ sidOf[t] /= 0 /\ cl[sidOf[t]] = t
    /\ cl' = [cl EXCEPT ![sidOf[t]] = "none"]
    /\ UNCHANGED <<TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig,
                   n, C, N>>

\* `kith AGENT --held` (M1-7, kernel/lib/kith-core.sh kith_held_launch): the launcher resolved and claimed; for a
\* `--resume` it gives the claim back only when the core is up, then terminal.open takes the claim again (kind
\* `starting`, retargeted to the stub's pid; refused while a take-over is queued, or while a warm process has the
\* conversation loaded) and starts the stub, which takes the lock. These are two steps with a gap: a third writer
\* that takes the claim in it makes open answer busy (75), and the run the user asked for is lost, never a second
\* engine. HandoverAtomic is the what-if: open takes over the launcher's claim by pid and start time.
TermHandBack(t) ==
    /\ HeldLaunch /\ ~HandoverAtomic /\ TS[t] = "launching" /\ sidOf[t] /= 0 /\ cl[sidOf[t]] = t
    /\ TS' = [TS EXCEPT ![t] = "handing"]
    /\ cl' = [cl EXCEPT ![sidOf[t]] = "none"]
    /\ UNCHANGED <<sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig, n, C, N>>

TermOpen(t) ==
    /\ TS[t] = "handing"
    /\ IF Free(sidOf[t]) /\ to = "none" /\ WarmOK(sidOf[t])
       THEN /\ TS' = [TS EXCEPT ![t] = "launching"] /\ cl' = [cl EXCEPT ![sidOf[t]] = t] /\ UNCHANGED ev
       ELSE /\ TS' = [TS EXCEPT ![t] = "gone"] /\ ev' = ev \cup {"lost"} /\ UNCHANGED cl
    /\ UNCHANGED <<sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig, n, C,
                   lk, eng, ot, na, gate, again, pend, ld>>

\* ---- the take-over (app asks the terminal to let go) ---------------------

KillTS(p) == IF TS[p] \in {"idle", "busy"} THEN [TS EXCEPT ![p] = "dying"] ELSE TS
KillSig(p) == IF TS[p] = "gone" THEN "foreign" ELSE "own"

Abort ==
    /\ taking' = FALSE /\ to' = "none"

\* S take_over(): taking.insert; take_over_inner: liveness of the host's copy: Closed -> no-op, Busy -> TerminalBusy,
\* Idle -> its take-over claim (<sid>.takeover.claim) and the .taken note.
TakeBegin ==
    /\ ~taking /\ to = "none" /\ infoPid /= "none"
    /\ \/ VisOf(infoPid) = "closed" /\ taking' = FALSE /\ to' = "none"
       \/ VisOf(infoPid) = "busy" /\ TakeoverRefusesBusy /\ taking' = FALSE /\ to' = "none"
       \/ (VisOf(infoPid) = "idle" \/ (VisOf(infoPid) = "busy" /\ ~TakeoverRefusesBusy))
          /\ taking' = TRUE /\ to' = "wrote"
    /\ UNCHANGED <<N, TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, lw, sig, n, C>>

\* S take_over_inner: "Re-checked just before the signal", then registry::terminate (SIGTERM). Two steps in the code.
TakeRecheck ==
    /\ to = "wrote"
    /\ IF TakeoverRechecks /\ VisOf(infoPid) /= "idle"
       THEN Abort /\ UNCHANGED <<TS, sig>>
       ELSE IF TakeoverAtomic
            THEN IF Runs(infoPid)
                 THEN /\ TS' = KillTS(infoPid) /\ sig' = KillSig(infoPid) /\ to' = "signalled" /\ UNCHANGED taking
                 ELSE Abort /\ UNCHANGED <<TS, sig>>
            ELSE /\ to' = "rechecked" /\ UNCHANGED <<taking, TS, sig>>
    /\ UNCHANGED <<N, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, lw, n, C>>

TakeSignal ==
    /\ to = "rechecked"
    /\ IF Runs(infoPid)
       THEN /\ TS' = KillTS(infoPid) /\ sig' = KillSig(infoPid) /\ to' = "signalled" /\ UNCHANGED taking
       ELSE Abort /\ UNCHANGED <<TS, sig>>
    /\ UNCHANGED <<N, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, lw, n, C>>

\* S take_over_inner: waits for the process to end (EXIT_WAIT) and the terminal's claim to go, then imports the last
\* lines and sets the writer; the take-over claim is given back (ReleaseOnDrop).
TakeFinish ==
    /\ to = "signalled" /\ ~Runs(infoPid)
    /\ taking' = FALSE /\ to' = "none" /\ lw' = "none"
    \* "The terminal wrote to the session: the actor loads it again" (kv_delete stale, then forget_loaded)
    /\ staleKv' = FALSE /\ forgotten' = (forgotten \/ Forgets)
    /\ UNCHANGED <<N, TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, sig, n, dirty>>

TakeTimeout ==
    /\ to = "signalled" /\ Runs(infoPid)
    /\ Abort
    /\ UNCHANGED <<N, TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, lw, sig, n, C>>

\* ---- the host's registry scan (every 3 s) ------------------------------

\* S scan() -> sync_session / mapped / adopt (copy the registry, move the lane to the terminal's session), import
\* (skipped while the host writes; forget_loaded when lines came) and update_state (liveWriter; `stale` +
\* forget_loaded when seen live), which a take-over in progress skips.
Scan ==
    /\ infoPid' = IF regPid /= "none" THEN regPid ELSE infoPid
    /\ infoSid' = IF regPid /= "none" THEN regSid ELSE infoSid
    /\ lane' = IF regPid /= "none" THEN regSid ELSE lane
    /\ lw' = IF taking \/ infoPid' = "none" THEN lw
             ELSE IF VisOf(infoPid') /= "closed" THEN "terminal"
             ELSE IF hr /= "none" THEN "app" ELSE "none"
    /\ LET sawLive == ~taking /\ infoPid' /= "none" /\ VisOf(infoPid') /= "closed" /\ ~staleKv
           adopted == regPid /= "none" /\ regSid /= lane
           imported == ForgetOnImport /\ regPid /= "none" /\ hr = "none" /\ dirty
       IN /\ staleKv' = (staleKv \/ sawLive \/ adopted)
          /\ forgotten' = (forgotten \/ ((sawLive \/ adopted \/ imported) /\ Forgets))
    /\ UNCHANGED <<N, TS, sidOf, cl, regPid, regSid, hooked, ghost, cur, gs, hq, H, taking, to, sig, n, dirty>>

Next ==
    \/ AppGate \/ BotGate_ \/ Enqueue \/ HostStart \/ HostWake \/ HostClaim \/ HostEnd \/ HostRestart
    \/ HostGateRead \/ HostSettleOwn \/ HostClaimNext \/ HostTake \/ HostRedeem \/ HostFinish \/ HostReport
    \/ HostGiveUp \/ HostAbandon \/ HostKill \/ HostSettle \/ HostKilled \/ EngineDied \/ HostDied
    \/ WarmKilled \/ StubExit \/ OffTurnWrite \/ OffTurnKilled \/ HostRestartCore \/ HostAdopt \/ OrphanGone \/ GateFlip
    \/ \E t \in Terminals :
          TermClaim(t) \/ TermResolve(t) \/ TermLaunch(t) \/ TermUp(t) \/ TermHook(t) \/ TermPrompt(t) \/ TermDone(t)
          \/ TermExit(t) \/ TermWrapperDies(t) \/ TermHandBack(t) \/ TermOpen(t)
    \/ TakeBegin \/ TakeRecheck \/ TakeSignal \/ TakeFinish \/ TakeTimeout
    \/ Scan

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* ======================== PROPERTIES ====================================

\* P1. At most one writer holds the thread: a live terminal session, or a host turn that has sent its prompt (CoreWriter:
\* or any engine process writing to it: an abandoned turn not gone yet, one a restart left, an off-turn wake).
\* With the core writer only a live engine counts: the host's "running" state is a record, not a writer, once its engine is gone.
OneWriter ==
    Cardinality(Holding) + (IF eng /= 0 \/ (~CoreWriter /\ hp = "running") THEN 1 ELSE 0) <= 1

\* P2. A take-over never ends a terminal that is working on a turn.
TakeoverOnlyWhenIdle == [][\A p \in Terminals : ~(TS[p] = "busy" /\ TS'[p] = "dying")]_vars

\* P2b. A take-over never signals a process that is not the session's (a reused pid).
NoForeignSignal == sig /= "foreign"

\* `kith <agent> --resume` passes resolve and goes on to start claude.
Resolves(t) == TS[t] = "claimed" /\ TS'[t] \in {"launching", "idle"}
\* Any Claude start through bin/kith goes ahead (resolve, or a launch resolve did not claim).
Launches(t) == TS[t] \in {"fresh", "claimed"} /\ TS'[t] \in {"launching", "idle"}

\* P3. `kith <agent> --resume` refuses while the host runs a turn in the thread.
ResumeRefusedWhileTurnRuns == [][\A t \in Terminals : Resolves(t) => hr = "none"]_vars

\* P3b. ... and while another terminal has the session open (as far as the registry file says).
ResumeRefusedWhileOpen == [][\A t \in Terminals : Resolves(t) => ~(regPid /= "none" /\ TS[regPid] \in Live)]_vars

Accepted(w) == Len(gs') = Len(gs) + 1 /\ gs'[Len(gs')] = w

\* P4. A send is refused (409 liveInTerminal) while the host's copy of the registry says the terminal is live
\* or a take-over runs.
SendRefusedWhenHostSeesTerminal ==
    [][Accepted("app") => ~(infoPid /= "none" /\ (taking \/ VisOf(infoPid) /= "closed"))]_vars

\* Strict forms: what is TRUE, not what the host's copy says.
\* S1. A send is never accepted while a terminal has the session open or is starting to.
SendNeverWhileTerminalClaims == [][Accepted("app") => Claiming = {}]_vars
\* S2. A terminal never passes resolve while a send is accepted but not finished (queued or past the gate).
ResumeNeverWhileSendPending == [][\A t \in Terminals : Resolves(t) => (hr = "none" /\ gs = <<>> /\ hq = <<>>)]_vars
\* S3. Two terminals never go ahead on the thread at once.
ResumeNeverWhileOtherTerminal == [][\A t \in Terminals : Launches(t) => Claiming \ {t} = {}]_vars

\* P5. A host turn that starts with no terminal holding the thread starts on the session the last writer left.
ContinuesLastSession == [][(hr = "none" /\ hr' /= "none" /\ Holding = {}) => lane' = cur']_vars

\* P7. A host turn never starts on a cached session the terminal has written to since: the actor reloads it.
CacheNeverStale == [][(hr = "none" /\ hr' /= "none" /\ Holding = {}) => ~dirty']_vars

\* P6. A bot's ask is refused while the thread is held by a terminal or being taken over.
BotRefusedWhileHeld == [][Accepted("bot") => (Claiming = {} /\ ~taking)]_vars

\* ---- CoreWriter: two layers (the claim row, the stub's lock), each one enough on its own where the other is absent.
\* These are safety rules about the modelled steps. Not claimed (liveness, they need the end report and the settle to
\* be delivered): that a lost end report, a claim `ending` on a gone stub or a refused hand-over is ever resolved.

\* C1. A live claim is never replaced by another claimant: the claimant must find the row free (claims_one_writer).
ClaimsExclusive ==
    [][\A s \in Sids : (cl[s] /= "none" /\ cl'[s] /= "none") => cl'[s] = cl[s]]_vars

\* C2. "Gone means empty" for the host: an engine process that writes (an off-turn wake aside) always has its claim row.
\* The claim outlives the process: it ends after the process is gone, never before.
ClaimOutlivesEngine == CoreWriter => (eng /= 0 /\ ~ot => HostKind(cl[eng]))

\* C3. ... or at least one layer: its claim row, or the stub's lock.
EngineCovered == CoreWriter => (eng /= 0 /\ ~ot => (HostKind(cl[eng]) \/ lk[eng] = "host"))

\* C4. A running turn's engine holds the lock (not while abandoned: that window is the claim's alone; not an off-turn wake).
LockCoversRunning == CoreWriter => ((hp = "running" /\ eng /= 0) => lk[eng] = "host")

\* C5. A live terminal has its claim row, or the lock (the owner on the lock alone after a no-answer; an interactive run
\* whose wrapper died).
TerminalCovered == CoreWriter => \A t \in Holding : (cl[sidOf[t]] = t \/ lk[sidOf[t]] = t)

\* C6. No begin goes on past the gate when its own gate read was not ok:true (the read and the claim are two steps in the
\* code, so a flip between them is not covered: the core does not check the switch, hostclaim.* is unswitched).
NoBeginWhileHeld == [][(hp = "gated" /\ hp' = "begun") => gate = "ok"]_vars

\* C7. A turn's end (reported, settled or died) leaves none of its own claim rows behind. A restart leaves some on
\* purpose (the old instance's, settled by adopt, a begin's settle or the core), so the check stops there.
NoLeakedClaim ==
    "restart" \notin ev => \A s \in Sids : ~(HostKind(cl[s]) /\ hp = "none" /\ eng /= s)

\* C8. A begin never stays queued behind a claim of its own that has no process any more.
NeverRefusedByDeadOwnClaim == "deadrefused" \notin ev

\* C9. --held: the run the user asked for is not lost to a writer that took the claim in the hand-over gap.
HandoverNeverRefused == [][\A t \in Terminals : ~(TS[t] = "handing" /\ TS'[t] = "gone")]_vars

\* ---- reachability (each is expected to FAIL: the state it denies can happen, so a pass elsewhere is not vacuous)
NeverHostRuns == hp /= "running"
NeverBotRuns == ~(hr = "bot" /\ hp = "running")
NeverTerminalBusy == \A t \in Terminals : TS[t] /= "busy"
NeverTookOver == ~(sig = "own" /\ to = "none")
NeverHostRunsAfterTerminalHeld == ~(hp = "running" /\ \E t \in Terminals : TS[t] = "gone" /\ t \in hooked)
NeverHostRefusedByClaim == ~(hp = "gated" /\ ~Free(lane))
\* CoreWriter
NeverLockBusy == "busy" \notin ev
NeverAbandonEnding == hp /= "ending"
NeverDied == "died" \notin ev
NeverKilled == "killed" \notin ev
NeverFailover == "failover" \notin ev
NeverOrphan == ~(hp = "none" /\ eng /= 0 /\ ~ot)
NeverSettledOwn == "settled" \notin ev
NeverOffTurnWrite == "offturn" \notin ev
NeverLockedOut == "lockout" \notin ev
NeverLockAlone == "lockalone" \notin ev
NeverRestartLocked == "restartlocked" \notin ev
NeverHeldGate == "held" \notin ev
NeverHandoverLost == "lost" \notin ev

=============================================================================
