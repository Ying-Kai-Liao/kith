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
    TakeoverRefusesBusy,   \* S take_over_inner: Live::Busy -> TerminalBusy
    TakeoverRechecks,      \* S take_over_inner: liveness again just before the signal
    ProcStartCheck,        \* R liveness/started_as: a reused pid is not the session
    ForgetsCache,          \* S forget_loaded -> Q session.rs forget_loaded: the actor drops its cached session
    ForgetOnImport,        \* S import_locked: an import that brings new lines forgets the cache, even if the
                           \* terminal was never seen live
    \* --- scope switches
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
               \* "launching" "idle" "busy" "dying" "gone"
    sidOf,     \* per terminal: the session id it writes (0: none yet); its claim (cl) is on the id bin/kith claimed
    cl,        \* per session id: who holds its writer claim <sid>.claim: a terminal, "app", "bot" (a host turn), "none"
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
    hp,        \* its phase: "none" "gated" (start_gate passed, no claim yet) "running" (claimed, prompt sent)
    hcl,       \* the session id the running host turn claimed (0: none)
    taking,    \* the thread is in Terminal.taking
    to,        \* take-over progress: "none" "wrote" "rechecked" "signalled" (not "none": its .takeover.claim is held)
    lw,        \* the summary's liveWriter field: "none" "terminal" "app" "bot"
    sig,       \* whom the last take-over signalled: "none" "own" "foreign"
    n,         \* sends made
    staleKv,   \* kv terminal.stale.<thread>: a terminal wrote since the host last loaded the session
    forgotten, \* the actor dropped its cached session (conn.loaded, thread_sessions): its next turn loads it again
    dirty      \* a terminal wrote to the session after the actor last loaded it (the cache is out of date)

C == <<staleKv, forgotten, dirty>>
H == <<hr, hp, hcl>>

vars == <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig, n, C>>

Sids == {1, 2}

\* Q session.rs forget_loaded(): "A turn running there is left alone."
Forgets == ForgetsCache /\ hp /= "running"

Live == {"idle", "busy", "dying"}
Holding == {t \in Terminals : TS[t] \in Live}
Claiming == {t \in Terminals : TS[t] \in Live \cup {"launching", "claimed"}}

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
\* that session (registry::read_one + liveness); last, the scanned copy's own liveness.
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
    /\ TS \in [Terminals -> {"fresh", "claimed", "launching", "idle", "busy", "dying", "gone"}]
    /\ sidOf \in [Terminals -> {0} \cup Sids] /\ cl \in [Sids -> Terminals \cup {"none", "app", "bot"}]
    /\ regPid \in Terminals \cup {"none"} /\ infoPid \in Terminals \cup {"none"}
    /\ hr \in {"none", "app", "bot"} /\ hp \in {"none", "gated", "running"} /\ hcl \in {0} \cup Sids
    /\ to \in {"none", "wrote", "rechecked", "signalled"}
    /\ lw \in {"none", "terminal", "app", "bot"} /\ sig \in {"none", "own", "foreign"}
    /\ Len(gs) + Len(hq) <= MaxSends /\ n <= MaxSends

-----------------------------------------------------------------------------
Init ==
    /\ n = 0 /\ gs = <<>> /\ hq = <<>> /\ hr = "none" /\ hp = "none" /\ hcl = 0 /\ taking = FALSE /\ to = "none"
    /\ sig = "none" /\ ghost = {} /\ cur = 1 /\ lane = 1 /\ regSid = 1 /\ infoSid = 1
    /\ forgotten = FALSE /\ dirty = FALSE
    /\ \/ \* a thread made in the app, or one older than the terminal feature: the host knows no terminal
          /\ TS = [t \in Terminals |-> "fresh"] /\ sidOf = [t \in Terminals |-> 0] /\ cl = [s \in Sids |-> "none"]
          /\ regPid = "none" /\ infoPid = "none" /\ lw = "none" /\ hooked = {} /\ staleKv = FALSE
       \/ \* a thread born in a terminal `kith <agent>` that is open now, hooked and already scanned; the run claimed
          \* its session id before claude started (FreshClaims)
          /\ StartTerminalBorn
          /\ TS = [t \in Terminals |-> IF t = "t1" THEN "idle" ELSE "fresh"]
          /\ sidOf = [t \in Terminals |-> IF t = "t1" THEN 1 ELSE 0]
          /\ cl = [s \in Sids |-> IF s = 1 /\ FreshClaims THEN "t1" ELSE "none"]
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
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, cur, hq, H, taking, to, lw, sig, dirty>>

\* api/botguests.rs ask -> S bot_gate(): writer() under the thread lock; a bot's own claim (its turn) lets it queue.
\* DESIGN "field": a gate reading the summary's liveWriter.
BotRefuses == IF BotGate = "field" THEN lw \in {"terminal", "app"} ELSE Writer = "terminal"
BotGate_ ==
    /\ BotEnabled /\ n < MaxSends /\ ~BotRefuses
    /\ gs' = Append(gs, "bot") /\ n' = n + 1
    /\ lw' = IF lw = "none" THEN "bot" ELSE lw
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, hq, H, taking, to, sig, C>>

\* api/bots.rs send: the user entry saved `queued`, then hub send_cmd(Cmd::Send) -> Q queue.rs on_cmd pushes
\* Queued::User. (Merged: resolve's send_queued sees the entry a moment before the queue does.)
Enqueue ==
    /\ gs /= <<>>
    /\ hq' = Append(hq, Head(gs)) /\ gs' = Tail(gs)
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, H, taking, to, lw, sig, n, C>>

\* Q queue.rs next_in_queue -> turn.rs start_turn_inner -> S start_gate(), under the thread lock: refused while
\* writer() is a terminal (the message fails with a notice); else `begin` marks the turn Working (working_lanes).
\* The turn then loads its session (a forgotten cache is loaded again). SyncLaneAtStart is a what-if.
HostStart ==
    /\ hr = "none" /\ hq /= <<>>
    /\ hq' = Tail(hq)
    /\ IF StartGateChecks /\ (IF SyncLaneAtStart THEN SyncedWriter ELSE Writer) = "terminal"
       THEN UNCHANGED <<H, lane, infoPid, infoSid, forgotten, dirty>>
       ELSE /\ hr' = Head(hq) /\ hp' = "gated" /\ UNCHANGED hcl
            /\ IF SyncLaneAtStart
               THEN lane' = SyncLane /\ infoPid' = SyncInfoPid /\ infoSid' = SyncInfoSid
               ELSE UNCHANGED <<lane, infoPid, infoSid>>
            \* a forgotten cache means this turn loads the session again, and so sees what the terminal wrote
            /\ IF forgotten \/ (SyncLaneAtStart /\ ForgetOnImport /\ Forgets /\ regPid /= "none" /\ dirty)
               THEN forgotten' = FALSE /\ dirty' = FALSE ELSE UNCHANGED <<forgotten, dirty>>
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, cur, gs, taking, to, lw, sig, n, staleKv>>

\* Q turn.rs claim_for_turn (after the session is loaded, before session/prompt): take_as(kind App, or Bot for a bot's
\* conversation) for the agent process. Held by another (a terminal, a one-shot) -> HeldByWriter, the turn fails.
HostClaim ==
    /\ hp = "gated"
    /\ IF Free(lane)
       THEN /\ cl' = [cl EXCEPT ![lane] = hr] /\ hp' = "running" /\ hcl' = lane /\ UNCHANGED hr
       ELSE IF HostStartCAS
            THEN /\ hr' = "none" /\ hp' = "none" /\ UNCHANGED <<cl, hcl>>
            ELSE /\ hp' = "running" /\ hcl' = lane /\ UNCHANGED <<cl, hr>>   \* broken: runs without the claim
    /\ UNCHANGED <<TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, taking, to, lw, sig, n, C>>

\* The writing turn's session is the lane's, which the last writer left.
HostWrites == cur' = lane

\* Q turn.rs finish (done, failed or Stop): the turn ends, release_turn_claim gives the claim back.
HostEnd ==
    /\ hp /= "none"
    /\ hr' = "none" /\ hp' = "none" /\ hcl' = 0
    /\ cl' = IF hcl /= 0 /\ cl[hcl] = hr THEN [cl EXCEPT ![hcl] = "none"] ELSE cl
    /\ IF hp = "running" THEN HostWrites ELSE UNCHANGED cur
    /\ UNCHANGED <<TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, lane, gs, hq, taking, to, lw, sig, n, C>>

\* The host process restarts: memory is lost (runtime, queue, taking); kv (info, lane, liveWriter) stays. The agent
\* process ends with it, and with it its turn claim; the host's take-over claim is stale (its pid is gone).
HostRestart ==
    /\ hr' = "none" /\ hp' = "none" /\ hcl' = 0 /\ hq' = <<>> /\ gs' = <<>> /\ taking' = FALSE /\ to' = "none"
    /\ cl' = [s \in Sids |-> IF cl[s] \in {"app", "bot"} THEN "none" ELSE cl[s]]
    /\ forgotten' = FALSE /\ dirty' = FALSE   \* no actor, no cache: the next turn loads the session
    /\ UNCHANGED <<TS, sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, lw, sig, n, staleKv>>

\* ---- the terminal side -------------------------------------------------

Up == IF LaunchIsInstant THEN "idle" ELSE "launching"

\* K kith-sessions resolve() (`kith <agent> --resume|-r|-c`, also a one-shot with a task): the host names the session
\* (the lane's), then claim(): take_claim (exclusive; a live claim of anyone refuses, a one-shot waits a little), and
\* a live take-over claim refuses. ResumeClaims = FALSE: no claim (a broken variant).
TermClaim(t) ==
    /\ TS[t] = "fresh" /\ to = "none"
    /\ ResumeClaims => Free(lane)
    /\ TS' = [TS EXCEPT ![t] = "claimed"] /\ sidOf' = [sidOf EXCEPT ![t] = lane]
    /\ cl' = IF ResumeClaims THEN [cl EXCEPT ![lane] = t] ELSE cl
    /\ UNCHANGED <<regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig, n, C>>

\* K resolve(), with the claim held: resolveThread again (S resolve: hostBusy, read under the thread lock, = a turn in
\* working_lanes, or a queued send, or `taking`), then live_pid of the registry file. A refusal dies and gives the
\* claim back (release_claims); else bin/kith goes on to start claude.
HostBusy == hr /= "none" \/ (HostBusyCountsQueued /\ (hq /= <<>> \/ taking)) \/ (SendQueuedUnderLock /\ gs /= <<>>)
TermResolve(t) ==
    /\ TS[t] = "claimed"
    /\ IF (ResumeChecksHostBusy /\ HostBusy) \/ (ResumeChecksOpen /\ regPid /= "none" /\ TS[regPid] \in Live)
       THEN /\ TS' = [TS EXCEPT ![t] = "gone"]
            /\ cl' = IF cl[sidOf[t]] = t THEN [cl EXCEPT ![sidOf[t]] = "none"] ELSE cl
       ELSE /\ TS' = [TS EXCEPT ![t] = Up] /\ UNCHANGED cl
    /\ UNCHANGED <<sidOf, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig, n, C>>

\* K bin/kith, a Claude start resolve did not claim: a run that continues the thread's session named by its caller
\* (KITH_RESUME: a channel, `kith ask -c`; the bridge's --resume in KITH_CLAUDE_ARGS). `kith-sessions terminal`
\* (interactive, refused at once) or `kith-sessions claim` (one-shot, waits a little) takes the claim for the launcher's
\* pid (exec / `run` keep it) before claude starts; take_claim reads only claims (a live take-over claim refuses), not
\* the registry, and the host is not asked. FreshClaims = FALSE: started without a claim (before #324; fresh-no-claim).
\* A fresh id or a fork at launch is a new session no thread names yet: it joins one only through the hook (TermHook).
TermLaunch(t) ==
    /\ TS[t] = "fresh"
    /\ FreshClaims => (Free(cur) /\ to = "none")
    /\ sidOf' = [sidOf EXCEPT ![t] = cur]
    /\ cl' = IF FreshClaims THEN [cl EXCEPT ![cur] = t] ELSE cl
    /\ TS' = [TS EXCEPT ![t] = Up]
    /\ UNCHANGED <<regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig, n, C>>

\* bin/kith -> kith-sessions run (or exec) -> claude starts and writes ~/.claude/sessions/<pid>.json: the session is open.
TermUp(t) ==
    /\ TS[t] = "launching"
    /\ TS' = [TS EXCEPT ![t] = "idle"]
    /\ UNCHANGED <<sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig, n, C>>

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
    /\ UNCHANGED <<TS, cl, ghost, infoPid, infoSid, lane, gs, hq, H, taking, to, lw, sig, n, C>>

\* The user types a prompt / the turn ends (claude's own status, read by R liveness: idle vs anything else).
TermPrompt(t) ==
    /\ TS[t] = "idle" /\ t \in hooked
    /\ TS' = [TS EXCEPT ![t] = "busy"] /\ dirty' = TRUE   \* the session now holds what the host's cache lacks
    /\ cur' = sidOf[t]
    /\ UNCHANGED <<sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, lane, gs, hq, H, taking, to, lw, sig, n, staleKv, forgotten>>

TermDone(t) ==
    /\ TS[t] = "busy"
    /\ TS' = [TS EXCEPT ![t] = "idle"]
    /\ UNCHANGED <<sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig, n, C>>

\* The user quits (or it crashes, or a refused launch dies). A crash can leave sessions/<pid>.json behind with the pid
\* reused (ghost). K run gives the claim back after claude exits; an exec'd one-shot's claim is stale with its pid.
TermExit(t) ==
    /\ TS[t] \in Live \cup {"launching", "claimed"}
    /\ TS' = [TS EXCEPT ![t] = "gone"]
    /\ cl' = [s \in Sids |-> IF cl[s] = t THEN "none" ELSE cl[s]]
    /\ \E g \in BOOLEAN : ghost' = IF g /\ TS[t] /= "claimed" THEN ghost \cup {t} ELSE ghost
    /\ UNCHANGED <<sidOf, regPid, regSid, hooked, infoPid, infoSid, cur, lane, gs, hq, H, taking, to, lw, sig, n, C>>

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
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, lw, sig, n, C>>

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
    /\ UNCHANGED <<sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, lw, n, C>>

TakeSignal ==
    /\ to = "rechecked"
    /\ IF Runs(infoPid)
       THEN /\ TS' = KillTS(infoPid) /\ sig' = KillSig(infoPid) /\ to' = "signalled" /\ UNCHANGED taking
       ELSE Abort /\ UNCHANGED <<TS, sig>>
    /\ UNCHANGED <<sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, lw, n, C>>

\* S take_over_inner: waits for the process to end (EXIT_WAIT) and the terminal's claim to go, then imports the last
\* lines and sets the writer; the take-over claim is given back (ReleaseOnDrop).
TakeFinish ==
    /\ to = "signalled" /\ ~Runs(infoPid)
    /\ taking' = FALSE /\ to' = "none" /\ lw' = "none"
    \* "The terminal wrote to the session: the actor loads it again" (kv_delete stale, then forget_loaded)
    /\ staleKv' = FALSE /\ forgotten' = (forgotten \/ Forgets)
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, sig, n, dirty>>

TakeTimeout ==
    /\ to = "signalled" /\ Runs(infoPid)
    /\ Abort
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, infoPid, infoSid, cur, lane, gs, hq, H, lw, sig, n, C>>

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
    /\ UNCHANGED <<TS, sidOf, cl, regPid, regSid, hooked, ghost, cur, gs, hq, H, taking, to, sig, n, dirty>>

Next ==
    \/ AppGate \/ BotGate_ \/ Enqueue \/ HostStart \/ HostClaim \/ HostEnd \/ HostRestart
    \/ \E t \in Terminals :
          TermClaim(t) \/ TermResolve(t) \/ TermLaunch(t) \/ TermUp(t) \/ TermHook(t) \/ TermPrompt(t) \/ TermDone(t)
          \/ TermExit(t)
    \/ TakeBegin \/ TakeRecheck \/ TakeSignal \/ TakeFinish \/ TakeTimeout
    \/ Scan

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* ======================== PROPERTIES ====================================

\* P1. At most one writer holds the thread: a live terminal session, or a host turn that has sent its prompt.
OneWriter == Cardinality(Holding) + (IF hp = "running" THEN 1 ELSE 0) <= 1

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

\* ---- reachability (each is expected to FAIL: the state it denies can happen, so a pass elsewhere is not vacuous)
NeverHostRuns == hp /= "running"
NeverBotRuns == ~(hr = "bot" /\ hp = "running")
NeverTerminalBusy == \A t \in Terminals : TS[t] /= "busy"
NeverTookOver == ~(sig = "own" /\ to = "none")
NeverHostRunsAfterTerminalHeld == ~(hp = "running" /\ \E t \in Terminals : TS[t] = "gone" /\ t \in hooked)
NeverHostRefusedByClaim == ~(hp = "gated" /\ ~Free(lane))

=============================================================================
