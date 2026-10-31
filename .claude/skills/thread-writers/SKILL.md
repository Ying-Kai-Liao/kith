---
name: thread-writers
description: Model-checks (TLA+/TLC) that one thread has one writer at a time. Use when a change touches kernel/bin/kith-sessions (resolve, the writer claims, record), the claim and `--resume` / take-over parts of kernel/bin/kith, the compact claim in kernel/bin/kith-lifecycle, serve/host/src/terminal/ (sync.rs, claim.rs, registry.rs), serve/host/src/api/threads.rs, the thread `send` in api/bots.rs, the bot guest gate in api/botguests.rs, the host turn queue, start and claim (`claim_for_turn`) in agent/bot/{queue,turn,session}.rs, working_lanes in hub/actors.rs, or when a new path starts Claude on a thread's session. Also use to answer "can two things write to this conversation?".
---

# Thread writers: one writer per thread

A thread is one Claude session. Four things can write to it: a terminal `kith <agent>` or a one-shot (including
`--resume`), an app `send` (the host runs a turn), the host's queue starting a turn later, and a bot
guest's `botAsk`. Each takes the session's writer claim (`terminal/claim.rs`, `kith-sessions take_claim`). Two
writers on one Claude session fork or corrupt the conversation. That is a question of the order of steps, so it is
checked with TLA+ (TLC), not with tests.

The model is `verification/threads/Threads.tla`. This skill is the only documentation; there is no README.

## Run it

```
verification/threads/run.sh                       # all rows of checks.tsv, about 25 s once the tools are cached
verification/threads/run.sh --show VARIANT PROP   # TLC's full output (the counterexample) for one row
verification/threads/drift.sh                     # which covered files changed since the sync commit
```

CI runs `run.sh` (job `thread-writers`) on pull requests and pushes that touch `verification/threads/`, this skill, or the covered code (the files `drift.sh` lists).

`run.sh` exits 0 only when every row of `verification/threads/checks.tsv` (variant, kind, property, expected
pass/fail) came out as expected. Tools are pinned and sha256-checked: `tla2tools.jar` v1.7.4 (MIT) and, only when
no Java 11+ is on PATH, Temurin JRE 21.0.9+10; both are cached under
`${XDG_CACHE_HOME:-$HOME/.cache}/kith-verify/` and nothing else is installed (no brew, nothing under /usr). On
macOS the `/usr/bin/java` stub does not count (`/usr/libexec/java_home` must succeed). In CI an existing Java is
used. First run downloads about 60 MB.

## Sync commit

**Last confirmed to describe the code: `ed2ceb6356599bbf65756709b3b91966c9ff8cc5`** (origin/main, after the writer
claims: #324 and its follow-ups, every Claude start through `kernel/bin/kith` claims its session before claude starts,
and a host turn takes the claim or is refused). The same sha is in `drift.sh` (`SYNC=`). Move it only after you have
confirmed that the model describes the code at the new commit (steps below), and change both places.

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
| `kith-sessions` `record()` (SessionStart hook, `write_atomic`, one file per session id, pid replaced by a later terminal; `resumedFrom` = `KITH_RESUMED_FROM`) | `TermHook` (incl. a new session id claude moved to: `ClaudeForks`) |
| claude's `~/.claude/sessions/<pid>.json` status idle vs other; user prompt and end | `TermPrompt`, `TermDone`, `TermExit` (a crash may leave a ghost: pid reused) |
| `terminal/sync.rs` `writer()`: `taking`; for the lane's session and the scanned copy's: `claim::holder` (not the host's own `App` claim; `Kind::Bot` -> `Writer::Bot`), then a live `registry::read_one`; last the copy's liveness | `Writer`, `WriterOf`, `RegLive` |
| `terminal/sync.rs` `send_gate_blocking()` (via `api/bots.rs` `send` -> `terminal::send_gate`, and `cards/resume.rs` `bot_door` -> `terminal_holds` for a wake), under `thread_lock`: `writer()` -> 409 (a bot guest's thread, an invited person's chat), or for the owner's own app chat `Waits` (the message is saved `queued` and enqueued as wake `msg:<entry>`; nothing is written to the session, so in the model this is the refused branch, with the same state; the wake's later delivery is a fresh `AppGate` then `Enqueue`, so it can never skip the gate); then `sync_one` (`mapped` adopts a fork, info, `import_locked` with `forget_loaded` on new lines), `stale` delete + `forget_loaded` | `AppGate` (`SendChecksLive`, `TakingBlocksSend`, `ForgetOnImport`) |
| `api/bots.rs` `send`: save the entry `queued` (after the gate, outside its lock), `send_cmd(Cmd::Send)` (or, when the gate said `Waits`, `resume::wait_for_terminal`: no actor, no turn; a wake whose gate is free sends `Cmd::Send` from `bot_door`; a start refused by `start_gate` hands its owner's user entries to the same wake); `agent/bot/queue.rs` `on_cmd` `Cmd::Send` pushes `Queued::User` | `Enqueue` (a second step after the gate) |
| `agent/bot/queue.rs` `next_in_queue` -> `turn.rs` `start_turn_inner` -> `sync.rs` `start_gate` (under `thread_lock`: `writer() == Terminal` refuses, else `begin` marks the turn Working) | `HostStart` (`StartGateChecks`) |
| `turn.rs` `claim_for_turn`: `claim::take_as` (`Kind::App`, or `Kind::Bot` for a bot's conversation) for the agent's pid; held by another -> `HeldByWriter`, the turn fails | `HostClaim` (`HostStartCAS`) |
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
| `kernel/bin/kith-lifecycle` `compact()` (F12): `parked()` forks the process that will be claude, held before its exec; `take_claim(sid, <that child's pid>, 'oneshot', lifecycle_owner=True)`, the checks again under it (`still`: decision unchanged, last turn unchanged, no live writer but that claim, not compacted since, over the floor), then the child execs `claude -p --resume SID /compact` (same pid, so the claim is claude's own and goes stale only with it, as a one-shot's through `kernel/bin/kith`; a sweep that dies before the go leaves a child that exits without exec), `release_claims(child)` after. A claim-only one-shot start of a session nobody holds; `lifecycle_owner` only keeps the intent and decision that `take_claim` clears for every other taker (rules 12, 14); the re-check only ever refuses more | `TermLaunch` (`FreshClaims`, one-shot kind), `TermUp`, `TermExit` |
| `kith-lifecycle` `end()`: `take_claim` for the sweep's own pid, `still`, writes the decision `ended`, `release_claims`. Starts no Claude and writes nothing to the session | none (not a writer) |

One step in the code is one action here. A check followed later by the act it guards is two actions
(`AppGate` then `Enqueue` then `HostStart` then `HostClaim`; `TermClaim` then `TermResolve` then `TermUp`;
`TakeRecheck` then `TakeSignal`), because another writer can act between them.

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

Fixed since the last sync (rows turned from `fail` to `pass`, evidence in the commits):
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
