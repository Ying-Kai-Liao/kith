#!/usr/bin/env bash
# Which code the model covers has changed since the model was last confirmed to describe it?
#
#   verification/threads/drift.sh        compares the working tree (so also uncommitted edits) with SYNC
#
# Prints the covered files that differ and a diff stat. No output of changed files = nothing to re-read.
# After you have read the changes, updated Threads.tla / base.consts if needed, and run.sh is as expected,
# set SYNC below to the commit you checked (and in .claude/skills/thread-writers/SKILL.md).
set -euo pipefail

# The commit at which the model was confirmed to describe the files below. It is one commit of origin/main:
# after the writer claims (#324: every Claude start claims its session; a host turn takes the claim or is refused).
SYNC=ed2ceb6356599bbf65756709b3b91966c9ff8cc5

FILES=(
  kernel/bin/kith-sessions            # record, resolve (claim, hostBusy, live_pid), take_claim, claim/terminal, run
  kernel/bin/kith                     # --resume: resolve + r_sid; csid: claim / terminal before claude; exec run
  serve/host/src/terminal/mod.rs      # Terminal.taking, HeldByWriter, TerminalBusy
  serve/host/src/terminal/registry.rs # liveness, started_as, terminate, pid_runs
  serve/host/src/terminal/sync.rs     # scan, sync_session, mapped, adopt, update_state, before_send, resolve, take_over
  serve/host/src/api/threads.rs       # resolveThread, takeOverThread
  serve/host/src/api/bots.rs          # send: terminal::before_send
  serve/host/src/agent/bot/queue.rs   # on_cmd: Cmd::Send, Cmd::ForgetLoaded; next_in_queue
  serve/host/src/agent/bot/turn.rs    # start_turn_inner: start_gate, claim_for_turn (the CAS), release_turn_claim
  serve/host/src/agent/bot/session.rs # forget_loaded
  serve/host/src/api/botguests.rs     # ask: terminal::bot_gate (the bot's writer gate)
  serve/host/src/terminal/claim.rs    # the writer claim: take_as, holder, is_live, Kind::Bot
  serve/host/src/hub/actors.rs        # working_lanes, Cmd::ForgetLoaded fan-out
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
