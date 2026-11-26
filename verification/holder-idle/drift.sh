#!/usr/bin/env bash
# Which code the model covers has changed since the model was last confirmed to describe it?
#
#   verification/holder-idle/drift.sh        compares the working tree (so also uncommitted edits) with SYNC
#
# Prints the covered files that differ and a diff stat. No output of changed files = nothing to re-read.
# After you have read the changes, updated WinKeeper.tla / the cfgs if needed, and run.sh is as expected,
# set SYNC below to the commit you checked (and in .claude/skills/holder-idle/SKILL.md).
set -euo pipefail

# The commit at which the model was confirmed to describe the files below (the fix: should_idle_out, sweep before record).
SYNC=5664661d92d536d250bab8231655ddf9f0379948

FILES=(
  core/holder/src/holder.rs           # should_idle_out, watch_idle, the ACK arm, drop_core, reap (sweep, then record)
  core/runtime/src/holder_link.rs     # open_claims, step 7 of reconcile, claims::is_reconciled, end_gone: when an unlisted claim ends
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
