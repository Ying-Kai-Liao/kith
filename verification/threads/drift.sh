#!/usr/bin/env bash
# Which code the model covers has changed since the model was last confirmed to describe it?
#
#   verification/threads/drift.sh        compares the working tree (so also uncommitted edits) with SYNC
#
# Prints the covered files that differ and a diff stat. No output of changed files = nothing to re-read.
# After you have read the changes, updated Threads.tla / base.consts if needed, and run.sh is as expected,
# set SYNC below to the commit you checked (and in .claude/skills/thread-writers/SKILL.md).
set -euo pipefail

# The commit at which the model was confirmed to describe the files below: origin/rust-rebuild after #1015 (rule K, the host
# runner of the core's queued turns) and #1016 (L-clear), re-read before the claims step runs on a Mac:
# every change to the files below up to it re-read, the claim files as before (#324) and the core writer (CoreWriter in
# Threads.tla: the claim is a core row, the stub's lock the second layer). What that read did not cover is in the skill.
SYNC=f9b5e509f476bb88f6203a125ae863369ba03aa9

FILES=(
  kernel/bin/kith-sessions            # record, resolve (claim, hostBusy, live_pid), take_claim, claim/terminal, run
  kernel/bin/kith                     # --resume: resolve + r_sid; csid: claim / terminal before claude; exec run
  kernel/bin/kith-lifecycle           # compact: take_claim for the parked child's pid, still(), then it execs claude; end
  kernel/bin/kith-wake                # deliver/drain: kith AGENT --resume SESSION (resolve claims), skip writer_live; take: host-door items
  serve/host/src/terminal/mod.rs      # Terminal.taking, HeldByWriter, TerminalBusy
  serve/host/src/terminal/registry.rs # liveness, started_as, terminate, pid_runs
  serve/host/src/terminal/sync.rs     # scan, sync_session, mapped, adopt, update_state, before_send, resolve, take_over
  serve/host/src/api/threads.rs       # resolveThread, takeOverThread
  serve/host/src/api/bots.rs          # send: terminal::before_send
  serve/host/src/agent/bot/queue.rs   # on_cmd: Cmd::Send, Cmd::WakeBatch, Cmd::ForgetLoaded; next_in_queue
  serve/host/src/agent/bot/turn.rs    # start_turn_inner: start_gate, claim_for_turn (the CAS), release_turn_claim
  serve/host/src/agent/bot/offturn.rs # adopt_off_turn: an agent that woke itself gets a turn behind start_gate + claim_for_turn
  serve/host/src/agent/bot/session.rs # forget_loaded
  serve/host/src/api/botguests.rs     # ask: terminal::bot_gate (the bot's writer gate)
  serve/host/src/cards/resume.rs      # handle: a waiting message's wake re-runs the send gate (terminal_holds); bot_door: Cmd::WakeBatch; run_door: kith --resume
  serve/host/src/terminal/claim.rs    # the writer claim: take_as, holder, is_live, Kind::Bot, Kind::Other (unknown kinds are live)
  core/kernel/src/threads.rs          # the Rust twin of kith-sessions (A4-2): take_in, resolve, claim_live, sweep_persons
  core/kernel/src/threads/os.rs       # pid_runs / start time the twin judges a claim by
  core/kernel/src/threads/cli.rs      # R5: the verbs bin/kith calls (claim, terminal, proc exit 9, epoch, release SID, sweep, lock-alone), in both modes
  core/kernel/src/threads/resolve.rs  # R5: resolve: the claim first, then the host's busy answer (a modelled row); the agent sent with the claim
  core/kernel/src/threads/core.rs     # R5: Writer mode = the claims marker or the switch file (fail closed), the core's three ends
  core/kernel/src/threads/writer.rs   # R5: Taker: core_take (revokeSeen, held / waiting / takeover, lock-alone for the owner only), epoch, release, sweep, proc (never held when down)
  core/kernel/src/threads/spool.rs    # R5: record: the spool first, session.record, drain, the marker re-check
  serve/host/src/threads_cmd.rs       # R5: the core client of the verbs (claim.* and session.record through core_call_within), the host's resolveThread
  serve/host/src/core_front.rs        # R5: core_call_dispatch / core_call_within: the daemon, else the embedded rows, with a deadline
  kernel/lib/core-switched.txt        # R5: the `threads` line is the switch of kith-sessions' verbs (one decision per launch in bin/kith)
  core/kernel/src/threads/registry.rs # the twin's registry reads and writes
  serve/host/src/hub/actors.rs        # working_lanes, Cmd::ForgetLoaded fan-out
  serve/host/src/terminal/core_writer.rs  # CoreWriter: begin (gate, settle, claim_next, take, redeem), finish, abandon, adopt
  serve/host/src/terminal/warm_stub.rs    # the host's resident stub: take / release / kill_group
  serve/host/src/agent/bot/writer.rs      # the actor's use of CoreWriter: orphaned begins, the mode
  serve/host/src/agent/bot/writer_turn.rs # End choice (Done / Died / Killed / OutOfUsage / Left), abandon + settle_gone, failover
  core/stub/src/resident.rs           # the stub's lock: take / release, nothing released on close; loaded / forgot markers
  core/stub/src/main.rs               # the one-shot take, then --lock-alone's clear before the engine
  core/stub/src/clear.rs              # L-clear: stale markers, a stub holding another key's lock (75), killpg + wait gone
  core/stub/src/loaded.rs             # the <key>.loaded/<stubpid>-<start> markers
  core/runtime/src/conversations/claims.rs   # claim_next, end_gone, the one live row per conversation
  core/runtime/src/conversations/external.rs # claim.external / claim_terminal: blocked_in (warm process), takeover
  core/runtime/src/conversations/warm.rs     # warm_procs: kill_at, adopt
  core/runtime/src/conversations/reconcile.rs # steps 5 and 7: rule K (a core restart keeps the host's live claims), not modelled here (Claims4)
  core/runtime/src/cutover/modules/claims.rs # the claims step: the takers' tombstone, then the files re-read, then the marker and Switched (the switch moment)
  serve/host/src/terminal/core_holders.rs # Writer mode readers: writer_folders, terminal_claim (the take-over's claim), Stop by claim id
  serve/host/src/terminal/core_claims.rs  # take_over / live_settled: the Writer-mode take-over through the core row
  serve/host/src/routines/core_runs.rs # the host runner: the core's queued turns begun by id on a fresh session (outside the thread)
  core/runtime/src/terminal.rs        # terminal.open: the --held claim and the stub's lock
  kernel/lib/kith_claims.py           # the kernel takers' route to the core claims
  kernel/lib/kith-core.sh             # kith_held_launch
)
paths=()
for f in "${FILES[@]}"; do paths+=("${f%% *}"); done

root=$(git rev-parse --show-toplevel)
cd "$root"
git cat-file -e "$SYNC^{commit}" 2>/dev/null || { echo "drift.sh: $SYNC is not in this clone; git fetch origin" >&2; exit 2; }
if git diff --quiet "$SYNC" -- "${paths[@]}"; then
  echo "no covered file differs from $SYNC: the model still describes them"
else
  echo "covered files that differ from $SYNC (read each change; see the skill, 'Update the model'):"
  git diff --stat "$SYNC" -- "${paths[@]}"
  echo
  echo "commits touching them since:"
  git log --oneline "$SYNC"..HEAD -- "${paths[@]}" || true
  exit 1
fi
