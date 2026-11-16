#!/usr/bin/env bash
# Which code the model covers has changed since the model was last confirmed to describe it?
#
#   verification/threads/drift.sh        compares the working tree (so also uncommitted edits) with SYNC
#
# Prints the covered files that differ and a diff stat. No output of changed files = nothing to re-read.
# After you have read the changes, updated Threads.tla / base.consts if needed, and run.sh is as expected,
# set SYNC below to the commit you checked (and in .claude/skills/thread-writers/SKILL.md).
set -euo pipefail

# The commit at which the model was confirmed to describe the files below. It is the L-clear commit (design 2026-11-16-offturn-lock-alone) on origin/rust-rebuild after M3 P9:
# every change to the files below up to it re-read, the claim files as before (#324) and the core writer (CoreWriter in
# Threads.tla: the claim is a core row, the stub's lock the second layer). What that read did not cover is in the skill.
SYNC=2fbb95d2efb14d33240d76f1182b6b7abbc8c693

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
