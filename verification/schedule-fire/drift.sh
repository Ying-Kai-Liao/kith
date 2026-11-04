#!/usr/bin/env bash
# Which code the model covers has changed since the model was last confirmed to describe it?
#
#   verification/schedule-fire/drift.sh        compares the working tree (so also uncommitted edits) with SYNC
#
# Prints the covered files that differ and a diff stat. No output of changed files = nothing to re-read.
# After you have read the changes, updated Fire.tla / base.consts if needed, and run.sh is as expected,
# set SYNC below to the commit you checked (and in .claude/skills/schedule-fire/SKILL.md).
set -euo pipefail

# The commit at which the model was confirmed to describe the files below. The model was written BEFORE the code
# (M4-3 does not exist yet): SYNC is origin/rust-rebuild when the architect's review was applied (the M3-2 turn API
# and the M4-2 store are on it; the code table in the skill cites their lines). When M4-3 lands, the first reader replaces this with the M4-3 merge commit after checking the code
# against the table in the skill.
SYNC=227257a53b10215197ef8e39e20bc854a6526fed

FILES=(
  kernel/bin/kith-sched               # tick(): the catch-up window, the paused skip, the "still running" skip (the rule M4-3 replaces)
  core/runtime/src/schedules          # M4-2 store and index; M4-3 will add tick, fire (the one transaction), run now, the webhook entry
  core/runtime/src/migrations/schedules  # sched_tick, sched_fires, sched_runs (unique keys)
  core/runtime/src/conversations/claims.rs  # enqueue_turn, claim, park, unpark_ready, expire_parked, revoke_person
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
