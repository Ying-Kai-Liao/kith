#!/usr/bin/env bash
# Which code the model covers has changed since the model was last confirmed to describe it?
#
#   verification/schedule-fire/drift.sh        compares the working tree (so also uncommitted edits) with SYNC
#
# Prints the covered files that differ and a diff stat. No output of changed files = nothing to re-read.
# After you have read the changes, updated Fire.tla / base.consts if needed, and run.sh is as expected,
# set SYNC below to the commit you checked (and in .claude/skills/schedule-fire/SKILL.md).
set -euo pipefail

# The commit at which the models were confirmed to describe the files below: origin/rust-rebuild after M4-5 packages A1
# (#1033), B (#1042), E (#1043) and C (#1044), with the code tables in the skill and the model headers checked.
SYNC=f4794190984e2d233e4547296198985428ef8584

FILES=(
  core/runtime/src/conversations/warm.rs  # adopt: the host's claims after a host restart (HostAdopt; package C changes it)
  core/runtime/src/hostclaim.rs  # hostclaim.queued, redeemedBefore (the runner's cancel of a redeemed turn)
  core/runtime/src/import/schedules.rs  # import of the schedule files into the index
  core/runtime/src/cutover/modules/schedules.rs  # the schedules cutover module
  core/runtime/src/cutover/modules/schedules_back.rs  # the rollback's hand-back (H4): next runs, state.json last
  core/runtime/src/cutover/modules/schedules_back_tests.rs  # its cases
  core/runtime/src/cutover/modules/schedules_switch.rs  # the step: host.lock, tick.lock (H1), the import (H2 anchor, H3 live run)
  core/runtime/src/cutover/modules/schedules_gate.rs  # gate conditions; condition 6 (package F) is Handover.tla's OldWait
  serve/host/src/agent/bot/writer_turn.rs  # HostRuns SeeDied: End::Died cancels a bot turn
  core/procstart  # the start stamps condition 6's lister of old ticks matches on (OldWait)
  kernel/bin/kith-sched               # tick(): the catch-up window, the paused skip, the "still running" skip (the rule M4-3 replaces)
  core/runtime/src/schedules          # M4-2 store and index; M4-3 tick, fire (the one transaction), run now, the webhook entry
  core/runtime/src/migrations/schedules  # sched_tick, sched_fires, sched_runs (unique keys)
  core/runtime/src/conversations/claims.rs  # enqueue_turn, claim, park, unpark_ready, expire_parked, revoke_person
  core/runtime/src/conversations/reconcile.rs  # Restart: step 5 (rule K keeps the host's live claims)
  serve/host/src/routines/core_runs.rs  # who runs a fired turn: the host's runner (hostclaim.queued, cancel a redeemed one)
  serve/host/src/routines/core_cron.rs  # a cron job's turn: claim it, start `kith-sched run-turn` detached, report its end (package C)
  core/runtime/src/conversations/sched_run.rs  # the cron run's claim: claim_run, attach (the check-and-set), paused_in (package C)
)
paths=()
for f in "${FILES[@]}"; do paths+=("${f%% *}"); done

root=$(git rev-parse --show-toplevel)
cd "$root"
git cat-file -e "$SYNC^{commit}" 2>/dev/null || { echo "drift.sh: $SYNC is not in this clone; git fetch origin" >&2; exit 2; }
if git diff --quiet "$SYNC" -- "${paths[@]}"; then
  echo "no covered file differs from $SYNC: the model still describes them"
else
  echo "covered files that differ from $SYNC (read each change; see the skill, 'When you change covered code'):"
  git diff --stat "$SYNC" -- "${paths[@]}"
  echo
  echo "commits touching them since:"
  git log --oneline "$SYNC"..HEAD -- "${paths[@]}" || true
  exit 1
fi
