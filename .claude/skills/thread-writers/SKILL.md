---
name: thread-writers
description: Model-checks (TLA+/TLC) that one thread has one writer at a time. Use when a change touches kernel/bin/kith-sessions (resolve, the writer claims, record), the claim and `--resume` / take-over parts of kernel/bin/kith, the compact claim in kernel/bin/kith-lifecycle, a wake's start in kernel/bin/kith-wake or cards/resume.rs, serve/host/src/terminal/ (sync.rs, claim.rs, registry.rs), serve/host/src/api/threads.rs, the thread `send` in api/bots.rs, the bot guest gate in api/botguests.rs, the host turn queue, start and claim (`claim_for_turn`) in agent/bot/{queue,turn,session}.rs, working_lanes in hub/actors.rs, or when a new path starts Claude on a thread's session. Also use to answer "can two things write to this conversation?".
---

# Thread writers: one writer per thread

A thread is one Claude session. Four things can write to it: a terminal `kith <agent>` or a one-shot (including
`--resume`), an app `send` (the host runs a turn), the host's queue starting a turn later, and a bot
guest's `botAsk`. A wake (`kernel/bin/kith-wake`, the host's card doors) starts one of these, never a fifth. Each takes the session's writer claim (`terminal/claim.rs`, `kith-sessions take_claim`). Two
writers on one Claude session fork or corrupt the conversation. That is a question of the order of steps, so it is
checked with TLA+ (TLC), not with tests.

The model is `verification/threads/Threads.tla`. This skill is the only documentation; there is no README.

## Run it

```
verification/threads/run.sh                       # all rows of checks.tsv, see the run time below
verification/threads/run.sh --slow                # the large rows of checks-slow.tsv, one at a time on every core
verification/threads/run.sh --show VARIANT PROP   # TLC's full output (the counterexample) for one row
verification/threads/drift.sh                     # which covered files changed since the sync commit
```

Run time: 101 rows, about 6 minutes on a laptop at 4 parallel jobs (P10, 2026-11-15): the nine passing `core` rows are 4.7M
states each (about 90 s), the slim scopes and the failing rows are under 200k. The job's timeout is 40 minutes.

`checks-slow.tsv` holds the large passing rows, same format: `run.sh --slow` runs them one at a time with TLC `-workers auto`
and a heap of `KITH_TLC_HEAP` (default 6g). Today one row, `core-held-cleared` `OneWriter` (L-clear, design 2026-11-16):
61,550,608 distinct states, depth 74, 13 min on 8 cores of an M-series laptop; one worker with 512 MB did not finish in 2 h.
CI job `thread-writers-slow` (4 cores, 10g heap, timeout 150 min) runs it under the same filter as `thread-writers`; a green
`ci` needs it to end `success`, a cancel or a timeout fails `ci`.

CI runs `run.sh` (job `thread-writers`) on pull requests and pushes that touch `verification/threads/`, this skill, or the covered code (the files `drift.sh` lists).

`run.sh` exits 0 only when every row of `verification/threads/checks.tsv` (variant, kind, property, expected
pass/fail) came out as expected. Tools are pinned and sha256-checked: `tla2tools.jar` v1.7.4 (MIT) and, only when
no Java 11+ is on PATH, Temurin JRE 21.0.9+10; both are cached under
`${XDG_CACHE_HOME:-$HOME/.cache}/kith-verify/` and nothing else is installed (no brew, nothing under /usr). On
macOS the `/usr/bin/java` stub does not count (`/usr/libexec/java_home` must succeed). In CI an existing Java is
used. First run downloads about 60 MB.

## Sync commit

**Last confirmed to describe the code: `e91223c03c138b859fbeaf7e688f5eab966d4a52`** (L-clear, design 2026-11-16-offturn-lock-alone, on origin/rust-rebuild
4192f0da1). The changes since `1978d087` were re-read: the core's claim list and reconcile check (`claims.rs`), the writer
mode re-checks, the abandon order (`stop`, `killpg`, `settle_gone`, already `AbandonKeepsLock`), the stray sweep and
`claim_since` (a reload after another claim) in `core_writer.rs` / `writer*.rs`: none changes who may write, so the model
is unchanged by them; the L-clear code (`kith-sessions` `LOCK_ALONE`, bin/kith `--lock-alone`, `core/stub/src/{clear,loaded,main}.rs`,
`writer.rs` `sync_warm`) is `LockAloneClearsWarm`.
The claim-file half of the model (`CoreWriter = FALSE`) was confirmed earlier at `6fc1ae0d` and its rows are unchanged; the
~1,500 lines since then in the covered files were read for what the model needs: the kernel takers' route to the core
(`kith-sessions` `core_take`, `kith_claims.py`), `--held`, the host writer (`core_writer.rs`, `writer*.rs`, `warm_stub.rs`),
the stub's lock (`resident.rs`) and the core's claim rows (`claims.rs`, `external.rs`, `warm.rs`). The Rust twin's
internals (`core/kernel/src/threads*`) were not re-read line by line: it is the same `take_claim` and is raced against the
Python by `kernel/tests/kith-threads-race`. The same sha is in `drift.sh` (`SYNC=`). Move it only after you have
confirmed that the model describes the code at the new commit (steps below), and change both places.

**The core writer (`CoreWriter = TRUE`, M3, P10).** With the claims module switched there is no claim file. The claim is a
core row (`hostclaim.*` for the host, `claim.external` / `claim_terminal` for kernel takers; `cl` stands for that table) and the
stub's flock (`lk`, `core/stub/src/resident.rs`) is a second, independent layer. Every taker claims first and locks second:
the host `claim_next`, then the stub's `take`, then `redeem`; a terminal `claim.external`, then the stub takes the lock when
claude starts (busy: exit 75, claim given back). Switch-by-switch: `HostTakesLock`, `TermTakesLock`, `TermRegistersClaim`,
`ClaimNextChecksHeld`, `GateRead`, `SettlesOwnEnding`, `DiedReports`, `FailoverReclaims`, `AbandonKeepsLock`, `AbandonSettlesEarly`,
`RestartKeepsClaim`, `ExternalChecksWarm`, `RequireStub`, `OwnerNoAnswer`, `LockAloneClearsWarm` (all TRUE / FALSE as the code is in
`base.consts`), and the what-if `ClearBeforeLock` (FALSE). The core rows run with one terminal and `MaxSends = 1` (`variants/core*.set`), about 5M states and 90 s
each for the full `core` scope; the smaller scopes (`core-gate`, `core-handover`, `core-forks`: no off-turn wakes, no
wrapper death, no owner-without-core) are tens of thousands. Every core row runs with `ClaudeForks = FALSE` except `core-forks`.

**Re-run for F7 (rust-rebuild, after #680):** `run.sh` at the F7 commit: 52 rows, every row as expected. Read: the F7 change to
`serve/host/src/terminal/claim.rs` (an unknown claim `kind` is `Kind::Other`: a live holder judged by pid + start only, and
`take_as` never removes it, as `kith-sessions` and the crate already do, so the "three actors" line below is true) and its
test; the Rust twin (`core/kernel/src/threads*`) is now in `drift.sh`'s file list. (SYNC moved by P10, see above.)

**`--held` (M1-7) is modelled** (`HeldLaunch`: `TermHandBack` then `TermOpen`, rows `core-handover`, `core-atomic-handover`). The
launcher gives its claim back, then `terminal.open` takes it for the stub. In that gap a third writer can take the claim:
`OneWriter` still holds (open answers busy, 75, nothing starts), but `HandoverNeverRefused` FAILS on the code: the run the user
asked for can be lost. That is still open (a known gap, not a bug in the model). The shortest trace takes no claim: a warm process
the app loaded during the gap makes `terminal.open` answer busy on `blocked_in`'s `waiting` (`terminal.rs`); retrying `waiting` in
open, as `claim_waiting` does, is a smaller fix than the atomic hand-over but covers only that case. The fix is the what-if `HandoverAtomic`
(`terminal.open` takes over the launcher's claim by pid + start time in one step instead of release-then-take), whose row
passes. When `terminal.open` answers direct or is refused, `kith-sessions terminal` takes the claim again (kith_held_launch):
the same take as open's, so it adds no behaviour to the model.

## What is promised, and the harm if not

| Property (in Threads.tla) | Plain sentence | Harm |
|---|---|---|
| `OneWriter` | At most one of: a live terminal session, a host turn that has sent its prompt, holds the thread. | Two Claude processes on one session fork or corrupt the conversation. |
| `TakeoverOnlyWhenIdle` | A take-over never SIGTERMs a terminal that is working on a turn. | The user loses the terminal's half-finished turn. |
| `NoForeignSignal` | A take-over never signals a process that is not the session's (reused pid). | An unrelated process is killed. |
| `ResumeRefusedWhileTurnRuns` | `kith <agent> --resume` refuses while the host runs a turn in that thread. | Terminal and app write at once. |
| `ResumeRefusedWhileOpen` | `--resume` refuses while the registry's terminal is live. | Two terminals on one session. |
| `SendRefusedWhenHostSeesTerminal` | A send is refused (409 `liveInTerminal`; for the owner's own app chat it waits as a wake and writes nothing) while the host's copy of the registry says the terminal is live or a take-over runs. | App writes into a terminal's session. |
| `ContinuesLastSession` | A host turn with no terminal open starts on the session the last writer left. | The app continues an old branch and the terminal's turns are lost from its context. |
| `CacheNeverStale` | A host turn never starts on a cached session the terminal has written to since (`forget_loaded`). Stated on the state after the start, so a sync inside the start counts. | The agent answers without seeing what the terminal did. |
| `ResumeNeverWhileSendPending`, `ResumeNeverWhileOtherTerminal`, `SendNeverWhileTerminalClaims` | The strict forms: refusals judged on what is true, not on what the checks can see. `ResumeNeverWhileOtherTerminal` covers every Claude start (resolve or a claim-only launch). | See the findings below. |
| `BotRefusedWhileHeld` | A bot's ask is refused while a terminal holds the thread or a take-over runs. | Bot and terminal write at once. |

**Core writer properties** (rows for `CoreWriter = TRUE`; safety rules about the modelled steps, each with a broken variant below):

| Property | Plain sentence | Harm |
|---|---|---|
| `OneWriter` (core) | Counts a live engine process writing: a turn, an abandoned one not gone yet, one a host restart left, an off-turn wake (the host's `running` state alone is not a writer once its engine is gone). | Two writers. |
| `ClaimOutlivesEngine` | A host engine that writes always has its claim row: the claim ends after the process is gone, never before ("gone means empty"). | A terminal or another turn claims under a live engine. |
| `EngineCovered` | ... or at least its lock. | |
| `LockCoversRunning` | A running turn's engine holds the stub's lock (not while abandoned, not an off-turn wake). | The second layer is missing. |
| `TerminalCovered` | A live terminal has its claim row or the lock (the owner on the lock alone when the core is not answering; an interactive run whose wrapper died). | |
| `ClaimsExclusive` | A live claim is never replaced by another claimant (`claims_one_writer`). Also true of the claim files. | Two claims on one conversation. |
| `NoBeginWhileHeld` | No begin goes on past the gate when its own gate read was not ok:true (Held). The read and the claim are two steps: a flip between them is not covered, and the core does not check the switch (`hostclaim.*` is unswitched). | A turn claims while the core is unclear. |
| `NoLeakedClaim`, `NeverRefusedByDeadOwnClaim` | A turn's end (reported, settled or died) leaves none of its own claim rows; a begin settles a claim of its own with no process left instead of queueing behind it. After a restart some are left on purpose (settled by adopt, a begin or the core), so `NoLeakedClaim` stops at a restart. | A conversation stuck behind a dead claim. |
| `HandoverNeverRefused` | `--held`: the run the user asked for is not lost to a writer that took the claim in the hand-over gap. | FAILS on the code (see above). |

Process identity is coarse: `ld` is "a warm process of this conversation" (a host restart's idle process is dropped at `adopt`, as it is
marked for a kill; the model does not name old and new processes). A gate rollback between the gate read and `claim_next` is not
modelled: the host would be on a core row while the kernel takers are back on the files, and only the lock covers it. `End::Killed`
goes through `acp.kill` (stdin closed, a grace, `killpg`), not `kill_group`; the routine-deadline kill works the same and ends `done`;
the model treats both as "dying, not writing".

Not claimed, because they are liveness rules (they need the end report, the settle or the retry tick to be delivered, and the model
has no fairness): a lost end report is eventually retried (`retry_end`), a claim `ending` on a gone stub is eventually settled, a
refused hand-over is eventually retried. The model also does not have: a stub crash between `take` and `redeem` (the code stops the
claim with the turn requeued and claims again; the claims model, Claims4, has it), the host's account pick (S1 not stepped), the
registration window of a fresh warm process (it has nothing to wake itself with), `End::Room` (the abandon is the same with the lock
kept), or the lock holder's identity beyond "a host stub" (an older host's stub is `lk = "host"` after a restart: the new host's
`take` sees busy until the old one is gone).

"Holds" = a terminal whose claude process is live (idle, busy or being ended). A host turn holds once it has its
claim and sends the prompt (`hp = "running"`). The `Never*` rows in `checks.tsv` are reachability probes: they must
FAIL, proving the runs can do a host turn, a bot turn, a busy terminal, a take-over, a host turn after a terminal, and
a turn refused by a held claim (a pass elsewhere is not vacuous).

## Coverage record (file -> function -> model action)

| Code | Model action |
|---|---|
| `kernel/bin/kith-sessions` `resolve()`: `host_call('resolveThread')` names the session, `claim(sid)` (`claim_waiting` -> `take_claim`: exclusive link, a live claim of anyone refuses, a one-shot waits `KITH_CLAIM_WAIT`; a live `.takeover.claim` refuses) | `TermClaim` (`ResumeClaims`) |
| `resolve()` with the claim held: `resolveThread` again, `hostBusy` die, `live_pid(e)` die; `die` gives the claim back (`release_claims`) | `TermResolve` (`ResumeChecksHostBusy`, `ResumeChecksOpen`) |
| `kernel/bin/kith` `csid` (the session the run continues, a `--session-id`, or a new uuid for a fresh run or a fork), then `kith-sessions claim` (one-shot) or `kith-sessions terminal` (interactive) when resolve did not claim it; neither asks the host nor reads the registry | `TermLaunch` (`FreshClaims`) |
| `exec claude` / `exec kith-sessions run -- claude` (run gives the claims back after claude exits; a one-shot's claim goes stale with its pid, `prune_stale`) | `TermUp`, `TermExit` (the launch gap: `LaunchIsInstant` removes it) |
| `kernel/bin/kith` `--held` (M1-7: `kith_held_launch` in `lib/kith-core.sh`, `kith-sessions release SID`, `core/runtime/src/terminal.rs` `open_held`): for a `--resume` the shell's claim is given back only when `terminal.list` says a holder is up and the claim's pid is the caller's parent with the same start time; `terminal.open` then takes the claim for the daemon's pid (`kind: starting`; also refused while a live `.takeover.claim` exists), and `retarget`s it to the stub's pid once spawned; busy answers 75 and starts nothing. When open answers direct or is refused, the claim is taken again by `kith-sessions terminal` and the run goes on with `kith-sessions run`. No daemon or no holder: the claim is never given back | `TermClaim` and `TermUp` by another actor (the core as claimer), and the gap below; **not modelled** |
| `kith-sessions` `record()` (SessionStart hook, `write_atomic`, one file per session id, pid replaced by a later terminal; `resumedFrom` = `KITH_RESUMED_FROM`) | `TermHook` (incl. a new session id claude moved to: `ClaudeForks`) |
| claude's `~/.claude/sessions/<pid>.json` status idle vs other; user prompt and end | `TermPrompt`, `TermDone`, `TermExit` (a crash may leave a ghost: pid reused) |
| `terminal/sync.rs` `writer()`: `taking`; for the lane's session and the scanned copy's: `claim::holder` (not the host's own `App` claim; `Kind::Bot` -> `Writer::Bot`), then a live `registry::read_one`; last the copy's liveness | `Writer`, `WriterOf`, `RegLive` |
| `terminal/sync.rs` `send_gate_blocking()` (via `api/bots.rs` `send` -> `terminal::send_gate`, and `cards/resume.rs` `bot_door` -> `terminal_holds` for a wake), under `thread_lock`: `writer()` -> 409 (a bot guest's thread, an invited person's chat), or for the owner's own app chat `Waits` (the message is saved `queued` and enqueued as wake `msg:<entry>`; nothing is written to the session, so in the model this is the refused branch, with the same state; the wake's later delivery is a fresh `AppGate` (`resume.rs` `handle` asks `terminal_holds` for a turn that carries a waiting message; held -> back to the wake queue) then `Enqueue` (`bot_door` -> `Cmd::WakeBatch`), so it can never skip the gate); then `sync_one` (`mapped` adopts a fork, info, `import_locked` with `forget_loaded` on new lines), `stale` delete + `forget_loaded` | `AppGate` (`SendChecksLive`, `TakingBlocksSend`, `ForgetOnImport`) |
| `api/bots.rs` `send`: save the entry `queued` (after the gate, outside its lock), `send_cmd(Cmd::Send)` (or, when the gate said `Waits`, `resume::wait_for_terminal`: no actor, no turn; a wake whose gate is free sends `Cmd::WakeBatch` from `bot_door`; a start refused by `start_gate` hands its owner's user entries to the same wake); `agent/bot/queue.rs` `on_cmd` `Cmd::Send` / `Cmd::WakeBatch` push `Queued::User` | `Enqueue` (a second step after the gate) |
| `agent/bot/queue.rs` `next_in_queue` -> `turn.rs` `start_turn_inner` -> `sync.rs` `start_gate` (under `thread_lock`: `writer() == Terminal` refuses, else `begin` marks the turn Working) | `HostStart` (`StartGateChecks`) |
| `turn.rs` `claim_for_turn`: `claim::take_as` (`Kind::App`, or `Kind::Bot` for a bot's conversation) for the agent's pid; held by another -> `HeldByWriter`, the turn fails | `HostClaim` (`HostStartCAS`) |
| `agent/bot/offturn.rs` `adopt_off_turn()` (an agent that woke itself, e.g. a finished background task: its `session/update` in `updates.rs`, or `send_file` / `send_message` through `ensure_turn`, with no turn running): the same `start_gate` under `thread_lock` and `claim_for_turn` as a queued turn, the runtime marked Working (`working_lanes`), no queue entry; ended by `end_off_turn` through `finish_turn` (quiet period, a message from anyone else, Stop, the agent exiting). Refused after Stop, a room, an ask or a routine | `HostWake` (`OffTurnWakes`), then `HostClaim`, `HostEnd` |
| `turn.rs` turn end / Stop, `release_turn_claim`; `hub/actors.rs` `working_lanes()` | `HostEnd`; `hr`, `hp` |
| `sync.rs` `resolve()` (`resolveThread`): `hostBusy` = `working_lanes` or `send_queued` or `taking`, read under `thread_lock`; `claims: 1` | `HostBusy` (`HostBusyCountsQueued`) |
| host restart | `HostRestart` (queue, `taking`, actor cache lost; kv stays; the agent process and its turn claim end with the host) |
| `terminal/sync.rs` `take_over()` / `take_over_inner()`: `taking.insert`, liveness of `info(...)`, the `.takeover.claim`, write `.taken` | `TakeBegin` |
| `take_over_inner`: "Re-checked just before the signal" | `TakeRecheck` |
| `registry::terminate` (SIGTERM, after the recheck) | `TakeSignal` (`TakeoverAtomic` merges it with the recheck: what-if) |
| `take_over_inner`: wait for `pid_runs` (`EXIT_WAIT`) and the terminal's claim, `import`, `stale` delete + `forget_loaded`, `set_writer` | `TakeFinish`, `TakeTimeout` |
| `registry.rs` `liveness()`, `started_as()` (procStart), `pid_runs()`; `claim.rs` `Claim::is_live` | `VisOf`, `Runs`, `ProcStartCheck`, `Free` |
| `terminal/sync.rs` `scan()` -> `sync_session` -> `mapped` / `adopt` (fork: lane session := terminal's), `import` (`forget_loaded` on new lines) and `update_state` (`stale` + `forget_loaded` when seen live; skipped while `taking`; `set_writer`) | `Scan` |
| `agent/bot/session.rs` `forget_loaded()` ("a turn running there is left alone"), `hub/actors.rs` `Cmd::ForgetLoaded` | `Forgets` |
| `api/botguests.rs` `ask` -> `terminal::bot_gate` (under `thread_lock`, `writer()`: a `Kind::Bot` claim is the bot's own turn and queues; a terminal refuses `heldByOtherWriter`); the claim at turn start as above; `liveWriter` is display only | `BotGate_` (`BotEnabled`, `BotGate = "live"`) |
| `kernel/bin/kith-lifecycle` `compact()` (F12): `parked()` forks the process that will be claude, held before its exec; `take_claim(sid, <that child's pid>, 'oneshot', lifecycle_owner=True)`, the checks again under it (`still`: decision unchanged, last turn unchanged, no live writer but that claim, not compacted since, over the floor), then the child execs `kith-stub ... -- claude -p --resume SID /compact` (same pid, so the claim is the stub's, which waits for claude as its child and goes stale only with it; the stub holds the conversation's lock meanwhile, and a run that already holds it makes the compaction `busy`, claim given back; a sweep that dies before the go leaves a child that exits without exec), `release_claims(child)` after. A claim-only one-shot start of a session nobody holds; `lifecycle_owner` only keeps the intent and decision that `take_claim` clears for every other taker (rules 12, 14); the re-check only ever refuses more | `TermLaunch` (`FreshClaims`, one-shot kind), `TermUp`, `TermExit` |
| `kernel/bin/kith-wake` `deliver()` (and `drain()`, which calls it per owed wake): skips a session whose `writer_live` holds, then runs `kith AGENT --resume SESSION NOTE` with `KITH_VIA=wake`, `KITH_CLAIM_WAIT=0`: the start goes through `resolve()`, which claims, so it is `TermClaim` then `TermResolve` like any `--resume`; refused (held, host busy, open) -> owed again, a held writer not counted as a failed try. `take()` hands host-door items to the host, see the next row. Writes nothing to the session itself | `TermClaim`, `TermResolve`, `TermUp` (no action of its own) |
| `cards/resume.rs` `handle()` (the host's wake doors): an `acp:` session -> `bot_door` (`Cmd::WakeBatch` onto the bot's queue, then `HostStart` / `HostClaim` as any queued turn; a refused start goes back to the wake queue held); a channel session -> `channel_door`; any other -> `run_door`: `claim::holder` held -> back to the queue, else it runs `kith AGENT --resume SESSION` (or `kith ask --reply ID AGENT --resume SESSION`), which claims through `resolve()` as in the row above. A card-only wake (no waiting message) is pushed with no gate, and `resolve()`'s `send_queued` does not count it while it waits; it is not modelled as its own step because no writer comes of it but through `HostStart` and the `HostClaim` CAS. Checked on a scratch copy (2026-11-01): a gate-free `WakeEnqueue` with `HostBusyCountsQueued = FALSE` leaves `TypeOK`, the three `code` rows and the four `code-no-fork` pass rows passing | `Enqueue` without `AppGate` (scratch only), then `HostStart`, `HostClaim`; `run_door` as `TermClaim` |
| `agent/bot/queue.rs` `Queued::Group` (group rooms, `botguests.rs` `ask_room` -> `chat::group::ask_agent`) and `agent/bot/hq.rs` (an hq lane loads its session after `claim_for_turn`): both start through `start_turn` / `claim_for_turn`; not thread lanes | none (outside the thread, claimed like any turn) |
| `kith-lifecycle` `end()`: `take_claim` for the sweep's own pid, `still`, writes the decision `ended`, `release_claims`. Starts no Claude and writes nothing to the session | none (not a writer) |

**Core writer (`CoreWriter = TRUE`)**

| Code | Model action |
|---|---|
| `agent/bot/hq.rs` / `offturn.rs` start + `ensure_hq_session`: the warm process spawned and registered (`warm_procs`) after `start_gate` | `HostStart`, `HostWake` (`ld`) |
| `core_writer.rs` `begin`: the gate read first (`Mode::select`; not ok:true is `Held`, the core asked nothing) | `HostGateRead` (`GateRead`, `GateFlips`) |
| `begin`: a stopped (`ending`) turn claim whose stub is gone is settled (`hostclaim.settle`, `proc_gone`), then the begin claims | `HostSettleOwn` (`SettlesOwnEnding`) |
| `attempt` step 3, `hostclaim.turn` = `claim_next` (held by any live claim: stays queued or parks) | `HostClaimNext` (`ClaimNextChecksHeld`) |
| step 5, the stub's `take` (busy: `report busy`, park `lock_busy`) | `HostTake` (`HostTakesLock`) |
| step 6 `redeem`, then the transcript sync under the lock and the prompt | `HostRedeem` |
| `finish` Done / OutOfUsage / Left: `release`, then the report; a move to the next account cancels the requeued core turn and begins a NEW one | `HostFinish`, `HostReport` (`FailoverReclaims`) |
| `finish` Killed (the 15 s kill): `exit` reported at once, the claim ends, the process group is killed (`acp.kill`: stdin closed, a grace, `killpg`; the routine-deadline kill is the same and ends `done`): dying, not writing, the lock until the stub exits | `HostKilled`, `StubExit` |
| `writer_turn.rs` abandon: `core.abandon` (`stop`, then `killpg` SIGKILL, then `settle_gone`; no `release`: the lock goes when the stub dies): the claim stays `ending` until the group is gone, the lock is held until then | `HostAbandon`, `HostKill`, `HostSettle`, `StubExit` (`AbandonKeepsLock`, `AbandonSettlesEarly`) |
| a begun turn given up before the prompt; a redeem the core refused | `HostGiveUp` |
| the process ends under its turn; `End::Died` reports `exit`, the item is rerun or cancelled | `EngineDied`, `HostDied` (`DiedReports`) |
| `offturn.rs`: an agent that woke itself writes before the gate and the begin, no claim, no lock; anything short of Running kills its group | `OffTurnWrite`, `OffTurnKilled` |
| `external.rs` `blocked_in`: `claim.external` answers `waiting` and marks the idle warm process for a kill | `WarmOK` (`ExternalChecksWarm`), `WarmKilled` |
| host restart: the stub keeps the engine and the lock, the claim row stays; `adopt` ends claims never redeemed and marks processes | `HostRestartCore` (`RestartKeepsClaim`), `HostAdopt`, `OrphanGone` |
| `kith-sessions` `core_take` (`claim.external`, `claim_terminal`), the core not answering: the owner's run goes on under the lock alone, marked so (`LOCK_ALONE`: `claim` / `terminal` print `lock-alone`, `resolve` adds `r_alone=1`; `kith_held_launch` sets `held_alone`) | `TermClaim`, `TermLaunch` (`OwnerNoAnswer`, gate held) |
| `writer.rs` `sync_warm` (from `warm_session`, before every `session/load`, and before each begin): with each `warm_procs` row the stub's ctl `loaded <key>` (`core/stub/src/resident.rs`, `loaded.rs`: `<locks>/<key>.loaded/<stubpid>-<start>`; a marker that cannot be made fails the load); with a row forgotten, `forgot`; the stub removes the rest when its engine has exited | `ld` (the one warm process's marker), `WarmOn` |
| `bin/kith` passes `--lock-alone` to `kith-stub`; `core/stub/src/main.rs` after the take, before the engine: `clear.rs` `clear` (stale marker removed; a marked stub holding another key's lock: exit 75; else `killpg` SIGKILL, wait gone, at most 5 s, else 75). No process is spawned under the lock (`the_clear_forks_nothing`) | `TermUp`'s locked branch (`LockAloneClearsWarm`, `StubLocksOther`, `ClearWarm`); `ClearBeforeLock` is the what-if at `TermClaim` / `TermLaunch` |
| `bin/kith` `exec kith-stub`: the lock taken when claude starts, busy = exit 75 and the claim given back; no stub program = unlocked unless `KITH_REQUIRE_STUB` | `TermUp` (`TermTakesLock`, `RequireStub`) |
| an interactive run's claim is on the `kith-sessions run` wrapper, which may die first (inferred, not run) | `TermWrapperDies` |
| `--held`: `kith_held_launch` gives the claim back, `terminal.open` takes it (`starting`, `retarget`), the stub locks | `TermHandBack`, `TermOpen` (`HeldLaunch`, `HandoverAtomic`) |

One step in the code is one action here. A check followed later by the act it guards is two actions
(`AppGate` then `Enqueue` then `HostStart` then `HostClaim`; `TermClaim` then `TermResolve` then `TermUp`;
`TakeRecheck` then `TakeSignal`), because another writer can act between them.

## The Rust twin (A4-2, not switched)

`core/kernel/src/threads*` (`kith kernel threads ...`) ports the kernel half of the table above step for step: the
same order in `resolve` (claim, then ask the host, then the checks), `take_in` = `take_claim` (temp file, lock file,
`link`, up to three judgements of a stale claim under the lock, the lock given up before `prune_stale`), `claim_start`,
`proc_claim`, `sweep_persons`, `record`. It writes the existing files with no version field, so Python, the host's
`claim.rs` and the crate are three actors on one folder, and the model's one-step `take` holds for each as long as all
use the lock. That is run, not modelled: `kernel/tests/kith-threads-race` races the Python `kith-sessions` and the Rust
verb for one thread (fresh, stale, reused pid, damaged, live) and fails on a second winner; CI runs it after the
difftest. Nothing calls the crate for claims yet (the router and the host switch together, later); the differences
from `kith-sessions` and `claim.rs` are in `docs/plans/one-bot-model.md` (A4-2).

## Bounds and assumptions

- One thread, two terminal runs (`t1`, `t2`, each runs once), at most 2 sends (`MaxSends`), two session ids (the
  thread's and one more that a fork moves to). Start state: an app thread the host knows no terminal for (also
  threads from before #128), or a thread born in a live terminal that claimed its session.
- A claim-only launch (`TermLaunch`) continues the session the thread's last writer left (`cur`): its caller named it
  (`KITH_RESUME`, `kith ask -c`, the bridge's `--resume`). A fresh id or a fork made at launch is a new session that
  no thread names; it joins one only through the hook (`resumedFrom`), which `TermHook` covers.
- `ClaudeForks` (scope, TRUE in `code`): claude may move to a new session id while it runs (`/clear`, `/branch`), and
  `record()` ties it to the thread through `KITH_RESUMED_FROM`; the claim stays on the id bin/kith claimed. `-no-fork`
  variants run the same code with claude keeping its id: that is where the claims' promise is checked.
- Time is not modeled: the 3 s scan interval, claude's start time, `KITH_CLAIM_WAIT` and the 5 s exit wait are "any
  order". So a finding is about an order that is possible, and the window can be milliseconds or seconds.
- Claims are judged live as `Claim::is_live` / `claim_live` do (pid runs, procStart matches): a terminal's claim goes
  with its run, a host turn's with the turn or the host process. Takers serialize on the claim's lock file
  (`take_claim`, `take_as`), so a take is one step.
- Assumed true and not checked: the transcript exists with a user message (`create_thread`); the registry file is
  written by the hook only for listable runs (`listable()`); import contents and cursors are right (this model is
  about who writes, not what is imported); routines, group rooms and other lanes of the bot are not thread lanes; the
  main lane, where `claim_for_turn` runs a turn without the claim when it is held, is not a thread.
- The kernel's `live_pid` has no procStart check; a reused pid there only makes `--resume` refuse (fail safe), so it
  is not modeled.
- Liveness (every send eventually runs or is refused) is not checked: it needs fairness assumptions that the code
  does not state.
- A pass covers this model at these bounds. It says nothing about the host's import, memory, or other agents.
- Bot guests: `BotGate = "live"` is the code's `bot_gate`; `design-bot-field` keeps the broken form (a gate that reads
  `liveWriter`). The same-lane queue (app and bot turns share one lane and one agent process) orders them inside the
  host; the claim keeps other processes out.

## Findings at the sync commit (the code breaks these; the rows in checks.tsv say `fail` on purpose)

All come from the code as it is; none was "fixed" in the model. `run.sh --show <variant> <PROPERTY>` prints each.

1. **A session id claude moves to carries no claim (`OneWriter`, `ResumeNeverWhileOtherTerminal`, `SendNeverWhileTerminalClaims`, `ContinuesLastSession` on `code`; `BotRefusedWhileHeld` on `code-claims-bot`).**
   A terminal run claimed session A; claude moves to B (`/clear`, `/branch`), and the hook ties B to the thread. B
   has no claim. A claim-only start of B (`kith ask -c B`, `KITH_RESUME=B`) asks neither the host nor the registry,
   takes B's claim and runs next to the terminal (`--resume` through `resolve` is refused by `live_pid`). Until a scan
   adopts B, the host's lane is still A, so a send or a bot ask is judged on A (free once the terminal has exited) and
   a turn may run on A while the terminal or the second run writes B (the thread splits). Window: the new run must name
   B, which only the registry and the thread show. Harm: two writers on B, or the app answering on the old branch.
2. **A send is accepted, then fails at its start (`ResumeNeverWhileSendPending`, `code-no-fork`).** `api/bots.rs`
   `send` saves the entry `queued` after `before_send` has released the thread lock, so `resolve` in between sees no
   queued send, claims and starts claude; the turn's `start_gate` then refuses (the message fails with a notice). Two
   writers no longer follow (`OneWriter` passes on `code-no-fork`). Window: microseconds to ms.
3. **SIGTERM after the recheck (`TakeoverOnlyWhenIdle`, `NoForeignSignal`).** `take_over_inner` re-checks "Idle",
   then calls `terminate`. If the user submits a prompt between the two (or the terminal exits and its pid is
   reused), the signal lands on a busy terminal or an unrelated process. Window: microseconds; `pidfd`-style
   atomicity is not available portably. Low likelihood, real harm if hit.
4. **A turn queued before a claim-only run starts on a stale cache (`CacheNeverStale`, `code-no-fork`).** A send is
   queued; a claim-only run of the same session (it does not ask the host, so the queued send does not stop it)
   writes and exits before the next scan; the turn then passes `start_gate` (no writer now), and nothing has imported
   the terminal's lines or forgotten the actor's cached session (`start_gate` does not `sync_one`). Window: one scan
   interval; the run must start and end while the send waits.

5. Closed (both halves), see "Fixed since the last sync" below: a run on the lock alone beside an off-turn wake (`core-held-cleared`).
6. **A terminal that moves to a new session id writes it under neither claim nor lock (`core-forks` `TerminalCovered`, `core-forks-two`
   `OneWriter`).** Finding 1 for the core writer: both layers are keyed by the id bin/kith claimed. With one terminal `OneWriter`
   holds at this scope; with two, the second takes the forked id and two terminals write.

Fixed since the last sync (rows turned from `fail` to `pass`, evidence in the commits):
- Finding 5, **with no core answering only the lock holds, and an engine that woke itself has none**. The owner's run goes on under the
  conversation lock alone when the core gives no answer (`core_take` `NoAnswer`); an agent that woke itself (an off-turn wake) writes
  with neither a claim nor the lock, before the host sees it, so a host-side rule (offturn.rs kills the group on any begin short of
  Running: `OffTurnKilled`) comes too late. Closed in the layer that holds without the core, the lock (L-clear, design
  `files/kith/designs/2026-11-16-offturn-lock-alone` of the architect): a resident stub marks every conversation its engine has loaded
  (ctl `loaded` / `forgot`, `<locks>/<key>.loaded/<stubpid>-<start>`, sent by the host's `sync_warm` with the `warm_procs` row, before
  `session/load`); `kith-sessions` marks the run lock-alone and bin/kith passes `--lock-alone`; the one-shot stub takes the lock and,
  holding it and before claude starts, clears each marker (`core/stub/src/clear.rs`): stale, removed; its stub holds another key's lock
  (a host turn runs there), exit 75; else SIGKILL its group and wait until it is gone (5 s, else 75). `core-held-cleared` `OneWriter`
  passes (checks-slow.tsv); `core-held` (`LockAloneClearsWarm = FALSE`, the code before) still fails, 9 steps as the architect's
  OffLock.tla: a turn, its end, an off-turn wake, the gate held, the owner's run on the lock alone. The order is the rule:
  `core-held-clear-before-lock` (`ClearBeforeLock`, the clear at the launch, before the lock) fails, because a host turn holding the
  lock at the launch is not cleared, ends, wakes itself, and the stub then takes the free lock. The model's clear is one step with the
  take (`TermUp`'s locked branch); in the code the stub holds the lock across it. The abandon half was closed before: an abandon is
  `stop`, `killpg` SIGKILL, `settle_gone`, with no lock release (`core-held-no-offturn` passes `OneWriter`; `core-held-old-abandon`,
  the lock given back first, fails it with the clear off, which would otherwise kill the abandoned engine too).
  Not covered by the clear: a run that goes on unclaimed because its id cannot be locked or its start read (`core_take`'s
  `unclaimable`: no lock either), a warm process started before this change (no markers), and a marked stub that takes another key's
  lock between the probe and the kill (that turn is killed: a lost turn, never a second writer).
- Old finding 1 (check, then act, with no claim) and 2 (the send gate trusting a copy from the next scan), on
  `code-no-fork`: every Claude start claims before claude starts (3cdd9a88, c2477a8b), `resolve` claims before it asks
  the host and the host reads `hostBusy` under the thread lock with queued sends and `taking` (3cdd9a88, 423df258),
  the gates read the claim and the registry on disk (`writer()`, 3cb64406), a host turn starts behind `start_gate` and
  takes the claim or is refused (`claim_for_turn`, 423df258, 7bc3296a). Old finding 4 (lane follows a fork late)
  remains only through finding 1 above.
- Old finding 5 (an import never forgot the cache): `import_locked` calls `forget_loaded` when it brings new lines
  (`ForgetOnImport` is TRUE in `base.consts`; `broken-no-import-forget` is the old form).
- Old finding 6 (no bot gate): `bot_gate` reads `writer()` and bot turns take a `Kind::Bot` claim (5a8553fa).

What fixes them (the `whatif-fixes` variant passes every property, with claude keeping its session id): the signal
tied to the checked process (`TakeoverAtomic`); `start_gate` running `sync_one` first, so a turn adopts, imports and
forgets before it is judged (`SyncLaneAtStart`); the send's entry saved `queued` inside `before_send`'s lock
(`SendQueuedUnderLock`). Finding 1 needs a claim that covers the thread, not one session id (the run claiming the
id claude moves to is not enough: the host's lane is judged on the old id until a scan); no what-if models that yet.
These are suggestions for the owners of the code, not changes made here.

## How we know it can fail

Each property fails on a broken variant (rows `broken-*` and `fresh-no-claim` in `checks.tsv`; read the
counterexample with `run.sh --show <variant> <property>`). The new ones keep claude on its session id
(`ClaudeForks = FALSE`), so the failure comes from the removed check alone:

| Variant | Removed | Fails |
|---|---|---|
| `fresh-no-claim` | the claim of a Claude start resolve did not claim (`kith-sessions terminal` / `claim` in bin/kith): a run started as before #324 | `OneWriter` (a second terminal on a terminal-born session) |
| `broken-host-no-cas` | the refusal in `claim_for_turn` (the turn runs though another holds the claim) | `OneWriter` (a one-shot claims between `start_gate` and the turn's claim) |
| `broken-resume-no-claim` | `claim(sid)` in `resolve` | `ResumeNeverWhileOtherTerminal` |
| `broken-no-send-gate` | the `writer()` check in `before_send_blocking` | `SendRefusedWhenHostSeesTerminal` |
| `broken-no-taking-block` | the `taking` check | `SendRefusedWhenHostSeesTerminal` |
| `broken-resume-ignores-host` | `hostBusy` die in `resolve` | `ResumeRefusedWhileTurnRuns` |
| `broken-resume-ignores-open` | `live_pid` die in `resolve` | `ResumeRefusedWhileOpen` |
| `broken-takeover-busy` | Busy refusal and recheck in `take_over_inner` | `TakeoverOnlyWhenIdle` |
| `broken-no-procstart` | procStart check in `started_as` (with atomic signal) | `NoForeignSignal` |
| `broken-no-import-forget` | `forget_loaded` in `import_locked` (with the fixes) | `CacheNeverStale` |
| `broken-no-forget` | `forget_loaded` (with the fixes) | `CacheNeverStale` |
| `core-restart-reports-done` | the old host instance's claim stays until its process is gone (HOST_INSTANCE as one constant read it as "ours": done) | `ClaimOutlivesEngine` |
| `core-failover-no-reclaim` | the new core turn of a move to the next account (the host's own retry runs on) | `EngineCovered` |
| `core-host-no-lock` | the host's stub `take` | `LockCoversRunning`, and `OneWriter` (a terminal whose wrapper died holds only the lock) |
| `core-term-no-claim-no-lock` | both layers for a terminal | `TerminalCovered`, `OneWriter` |
| `core-term-no-claim`, `core-term-no-lock`, `core-unlocked-owner` | one layer for a terminal (no claim row; no lock; no stub program) | `OneWriter`: neither layer is redundant. The lock covers a terminal whose wrapper died (its claim ended while claude runs) and an owner with the core down; a terminal with no claim row is kept out of an abandoned turn by the lock it still holds, and by the warm-process check |
| `core-abandon-settles-early` | the claim staying `ending` until the group is gone | `ClaimOutlivesEngine`. `OneWriter` still passes: the warm-process check in `claim.external` (the abandoned process is still registered) also keeps a terminal out |
| `core-held-old-abandon` | the abandon keeping the lock until the stub dies (the old order gave it back first) | `OneWriter`, with the core not answering (no off-turn wakes, and the lock-alone clear off, so only the abandon window is left) |
| `core-held` | the lock-alone clear (`LockAloneClearsWarm`: the code before L-clear) | `OneWriter` (an owner run on the lock alone next to an off-turn wake) |
| `core-held-clear-before-lock` | the clear's order: at the launch, before the lock (`ClearBeforeLock`) | `OneWriter` (a host turn that holds the lock at the launch is not cleared, then wakes itself) |
| `core-external-ignores-warm` | the warm-process check in `claim.external` | `OneWriter` (a terminal next to an off-turn wake) |
| `core-claim-next-ignores-held` | the `held` answer of `claim_next` (with the warm check also off, which otherwise keeps the terminal out of the window) | `ClaimsExclusive` |
| `core-died-no-report` | the `exit` report of `End::Died` | `NoLeakedClaim` |
| `core-no-gate-read` | the gate read at the top of `begin` | `NoBeginWhileHeld` |
| `core-no-settle-own` | the settle of our own `ending` claim on a gone stub | `NeverRefusedByDeadOwnClaim` |
| `code` (as is) | nothing: the findings above | `OneWriter` and the strict rows |

`start_gate` has no row of its own: with the claim CAS in `claim_for_turn`, removing it (`StartGateChecks = FALSE`)
still passes `OneWriter` in this model. It refuses earlier, with the notice the app shows, and checks the registry
as well as the claim; the CAS is what keeps the second writer out.

Also proved on the real code (scratch edits, not committed): at b523caf7, removing the `hostBusy` die in
`kernel/bin/kith-sessions` and the live check in `terminal/sync.rs` `before_send_blocking`; at ed2ceb63, removing
the one-shot `kith-sessions claim` in `kernel/bin/kith` and the `HeldByWriter` refusal in `turn.rs`
`claim_for_turn`. Each made `drift.sh` list the file; after setting the matching switch in `base.consts`
(`ResumeChecksHostBusy`, `SendChecksLive`, `FreshClaims`, `HostStartCAS` = FALSE), `run.sh` failed the rows that
guard it (`code-no-fork` `OneWriter` among them); all were then dropped.

## When you change covered code

1. `verification/threads/drift.sh`. Nothing listed: no work. Listed files: read each diff and the commit messages
   (`git log --oneline <SYNC>..HEAD -- <file>`), including code that moved to other files and new code that writes
   the guarded state (the registry, `taking`, the lane session, `liveWriter`, `Cmd::Send`).
2. Decide per change: no effect on the model; the model no longer matches (a step split in two, a check moved or
   removed, a new path to start a turn or a terminal); or the promise changed on purpose (cite the doc comment,
   test or commit). Update `Threads.tla` and `base.consts` from the code, not from what makes a result pass. Keep
   one step in code = one action; cite the function in the comment above the action.
3. `verification/threads/run.sh`. A row that differs is either drift (the model is wrong: replay the TLC
   counterexample step by step against the code; if a step cannot happen, fix the model and say what was wrong) or
   a bug (every step can happen: report it as a finding with the steps in the code's words and the harm; do not
   weaken the property or the model, and do not change product code unless asked). A finding that the code now
   fixes turns its row from `fail` to `pass`: change `checks.tsv` only with that evidence, and move the finding out
   of the list above.
4. A pass on updated code can have lost the ability to fail: break the new check (remove it or reorder the step)
   in a scratch edit and see `run.sh` fail; then drop the edit. Add a `broken-*` variant for a new check.
5. New writer or state: add the property, a `broken-*` variant that fails it, and a row each.
6. Only then set `SYNC` in `drift.sh` and the sync commit above to the commit you checked.

Not a bug: a counterexample that needs a step the code cannot take (for example a state the scan or hook can
never produce). That is a modeling error: fix the model.

For upkeep after merges that skipped this skill, follow `/maintain-formal-verification-skill` (pstack): find the
commits that touched covered code, update the model from the code, rerun, report each failure as drift or bug.

## Map of the folder

- `Threads.tla`: the model (actions cite code; properties at the end).
- `base.consts`: the switches for the code as it is. `variants/*.set`: overrides per variant (what-ifs, broken variants).
- `checks.tsv`: the table `run.sh` enforces. `run.sh`: pinned tools and the table. `drift.sh`: sync commit and covered files.

## Room-person sessions (2026-11-14, rooms with people, PR C) - prose only, no model row
Not a checked row: `Threads.tla` has no action for a room-person session and `checks.tsv` has no row for one, so the model says
nothing about it. This section describes the code. What holds it: `api/room_turn_tests.rs` (the actor key and the session owner)
and `verification/rooms/` (`steer`, `warmreuse`) for what a turn reads.
A room turn runs on an actor of its own (`hub/actors.rs`, key `bot|lane#room-<room>-<join gen>`) with a session of its own
(`session_owner`): one writer per (agent, room, join epoch), claimed through the same `claim_for_turn` as any person's. Nothing
is steered into a running turn, and its session is never loaded again after the next join. Drift was read; SYNC not moved.
